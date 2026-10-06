-- | Extract Core bindings from "fat" interface files (.hi compiled with
-- -fwrite-if-simplified-core). These contain mi_extra_decls: the full
-- post-optimization Core for ALL bindings including workers, loop-breakers,
-- and internal helpers that don't get normal unfoldings.
--
-- Uses findAndReadIface to read .hi files directly from disk, bypassing the
-- PIT (Package Interface Table) cache. The PIT replaces mi_extra_decls with
-- a panic thunk to save memory, so loadSysInterface can't be used here.
module Tidepool.FatIface
  ( FatIfaceCache, newFatIfaceCache, copyFatIfaceCache, mergeFatIfaceCaches, selectFatIfaceCaches, evictFatIfaceMatching
  , FatIfaceLookup(..), FatIfaceMissing(..), lookupFatIfaceExact, lookupFatIfaceBodies
  , ExactInterfaceFailure(..), readExactInterface
  , OwnerInterfaceContext(..), OwnerInterfaceCache, newOwnerInterfaceCache
  , copyOwnerInterfaceCache, mergeOwnerInterfaceCaches, selectOwnerInterfaceCaches, lookupOwnerInterface
  , cacheOwnerInterface, evictOwnerInterfaceMatching
  ) where

import GHC.Core (CoreBind, Bind(..))
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Core.TyCon (TyCon)
import GHC.Driver.Env (HscEnv, hsc_NC, hsc_dflags)
import GHC.Types.Name (Name, nameModule_maybe, isExternalName)
import GHC.Types.Var (Id, isId)
import GHC.Types.Var (varName)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Unit.Types (Module, moduleUnit, moduleName, mkModule, toUnitId)
import GHC.Unit.Module.ModIface (ModIface, mi_extra_decls)
import GHC.Utils.Outputable (showSDocUnsafe, ppr, text)

import GHC.Iface.Load (findAndReadIface, readIface)
import GHC.Iface.Errors.Types
  ( MissingInterfaceError(..), ReadInterfaceError(..) )
import GHC.IfaceToCore (tcTopIfaceBindings)
import GHC.Tc.Utils.Monad (initIfaceCheck, initIfaceLcl)
import GHC.Types.TypeEnv (emptyTypeEnv)
import GHC.Data.Maybe (MaybeErr(..))
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Unit.Module.Location (ModLocation, ml_hi_file)

import Control.Concurrent.MVar
  (MVar, modifyMVar_, newMVar, readMVar)
import Control.Exception
  ( displayException )
import Tidepool.FatIface.Internal qualified as Shared
import Tidepool.ExtractUtil (trySynchronous)
import Control.Monad.IO.Class (liftIO)
import Data.IORef (newIORef)
import qualified Data.Map.Strict as Map
import qualified Data.IntMap.Strict as IntMap
import qualified Data.Set as Set
import System.IO (hPutStrLn, stderr)
import System.Environment (lookupEnv)

-- | Exact recovery keeps absence distinct from an unreadable interface.
-- Missing definitions never become optimizer-unfolding executable bodies.
data FatIfaceMissing = NameWithoutModule | NoExtraDeclarations | BindingAbsent
  deriving (Eq, Show)

data FatIfaceLookup
  = FatIfaceFound [CoreBind]
  | FatIfaceMissing FatIfaceMissing
  | FatIfaceLoadFailure Module String

-- | An interface load is cached as an outcome, not as a map.  In particular,
-- an unreadable interface and an interface without extra declarations must not
-- become indistinguishable from a successfully loaded interface with no
-- matching binding.
data FatIfaceModule
  = FatIfaceBindings (IntMap.IntMap CoreBind) (Map.Map Name Int)
  | FatIfaceNoExtraDeclarations
  | FatIfaceLoadFailureOutcome String

data ExactInterfaceFailure
  = ExactInterfaceFinderFailure String
  | ExactInterfaceReadFailure String
  deriving (Eq, Show)

lookupFatIfaceExact :: HscEnv -> FatIfaceCache -> Name -> IO FatIfaceLookup
lookupFatIfaceExact hscEnv cache name = case nameModule_maybe name of
  Nothing -> pure (FatIfaceMissing NameWithoutModule)
  Just modl -> lookupFatIfaceBodies hscEnv cache modl [name]

-- | Select original groups in defining order, including their private top
-- scope. GHC's fat decoder gives unadvertised tops internal Names, so they
-- cannot re-enter external-name recovery. External siblings remain ordinary
-- demand edges; this operation never prepares the whole defining module.
lookupFatIfaceBodies :: HscEnv -> FatIfaceCache -> Module -> [Name] -> IO FatIfaceLookup
lookupFatIfaceBodies hscEnv cache owner requested = do
  outcome <- lookupModuleOutcome hscEnv cache owner
  pure $ case outcome of
    FatIfaceBindings groups names -> case traverse (`Map.lookup` names) requested of
      Nothing -> FatIfaceMissing BindingAbsent
      Just roots ->
        let selected = close Set.empty roots
            close seen [] = seen
            close seen (ordinal : pending)
              | ordinal `Set.member` seen = close seen pending
              | otherwise = close (Set.insert ordinal seen)
                  (privateDependencies (groups IntMap.! ordinal) ++ pending)
            -- Every pending ordinal comes from this immutable index, which
            -- is constructed alongside the complete original group table.
            privateDependencies binding =
              [ dependency
              | rhs <- case binding of NonRec _ body -> [body]; Rec pairs -> map snd pairs
              , identifier <- nonDetEltsUniqSet (exprSomeFreeVars privateId rhs)
              , Just dependency <- [Map.lookup (varName identifier) names] ]
            privateId identifier = isId identifier && not (isExternalName (varName identifier))
        in FatIfaceFound [binding | (ordinal, binding) <- IntMap.toAscList groups
             , ordinal `Set.member` selected]
    FatIfaceNoExtraDeclarations -> FatIfaceMissing NoExtraDeclarations
    FatIfaceLoadFailureOutcome reason -> FatIfaceLoadFailure owner reason

-- | Each stable resolution context owns its decoded Core cache. Sharing
-- completion is an internal loading mechanism; callers cannot supply bodies.
newtype FatIfaceCache = FatIfaceCache (Shared.LoadCache Module FatIfaceModule)

newFatIfaceCache :: IO FatIfaceCache
newFatIfaceCache = FatIfaceCache <$> Shared.newLoadCache

copyFatIfaceCache :: FatIfaceCache -> IO FatIfaceCache
copyFatIfaceCache (FatIfaceCache cache) = FatIfaceCache <$> Shared.copyLoadCache cache

mergeFatIfaceCaches :: [(FatIfaceCache, Module -> Bool)] -> IO FatIfaceCache
mergeFatIfaceCaches sources = FatIfaceCache <$> Shared.mergeLoadCaches
  [(cache, keep) | (FatIfaceCache cache, keep) <- sources]

selectFatIfaceCaches :: [(FatIfaceCache, Set.Set Module)] -> IO FatIfaceCache
selectFatIfaceCaches sources = FatIfaceCache <$> Shared.selectLoadCaches
  [(cache, owners) | (FatIfaceCache cache, owners) <- sources]

-- | Request-owned targets and changed private contexts can evict acceleration
-- without replacing another in-flight generation's eventual publication.
evictFatIfaceMatching :: FatIfaceCache -> (Module -> Bool) -> IO ()
evictFatIfaceMatching (FatIfaceCache cache) = Shared.evictLoadCache cache

lookupModuleOutcome :: HscEnv -> FatIfaceCache -> Module -> IO FatIfaceModule
lookupModuleOutcome env (FatIfaceCache cache) owner =
  Shared.lookupLoadCache cache owner (loadModuleExtraDecls env owner)

-- | Load and deserialize mi_extra_decls for a single module, retaining the
-- exact outcome for all selected-body callers.
-- Uses findAndReadIface to bypass the PIT cache (which strips mi_extra_decls).
loadModuleExtraDecls :: HscEnv -> Module -> IO FatIfaceModule
loadModuleExtraDecls hscEnv modl = do
  result <- trySynchronous (loadModuleExtraDeclsUnsafe hscEnv modl)
  case result of
    Right outcome -> return outcome
    Left e -> do
      ifaceDbg <- lookupEnv "TIDEPOOL_IFACE_DEBUG"
      case ifaceDbg of
        Just _ -> hPutStrLn stderr $
          "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": exception: " ++ show e
        Nothing -> pure ()
      return (FatIfaceLoadFailureOutcome (show e))

-- | Read an already-resolved defining identity, including hidden package
-- modules. Import visibility is not a condition for preparing a recovered
-- implementation. Preserve its exact location for CorePrep; home interfaces
-- use the finder's recorded location, never a reconstructed source path.
readExactInterface :: HscEnv -> Module
  -> IO (Either ExactInterfaceFailure (ModIface, ModLocation))
readExactInterface env owner = do
  let installed = mkModule (toUnitId (moduleUnit owner)) (moduleName owner)
  attempted <- trySynchronous $ findAndReadIface env
    (text "Tidepool exact defining interface") installed owner NotBoot
  case attempted of
    Left exception -> pure (Left
      (ExactInterfaceFinderFailure (displayException exception)))
    Right (Succeeded pair) -> pure (Right pair)
    Right (Failed (HomeModError _ location)) -> do
      raw <- trySynchronous $ readIface (hsc_dflags env) (hsc_NC env)
        owner (ml_hi_file location)
      pure $ case raw of
        Left exception -> Left (ExactInterfaceReadFailure (displayException exception))
        Right (Succeeded iface) -> Right (iface, location)
        Right (Failed failure) -> Left
          (ExactInterfaceReadFailure (renderReadInterfaceError failure))
    Right (Failed failure) -> pure $ Left $ case failure of
      BadIfaceFile{} -> ExactInterfaceReadFailure
        (renderMissingInterfaceError failure)
      DynamicHashMismatchError{} -> ExactInterfaceReadFailure
        (renderMissingInterfaceError failure)
      FailedToLoadDynamicInterface{} -> ExactInterfaceReadFailure
        (renderMissingInterfaceError failure)
      _ -> ExactInterfaceFinderFailure (renderMissingInterfaceError failure)

loadModuleExtraDeclsUnsafe :: HscEnv -> Module -> IO FatIfaceModule
loadModuleExtraDeclsUnsafe hscEnv modl = do
  ifaceDbg <- lookupEnv "TIDEPOOL_IFACE_DEBUG"
  let doc = text "tidepool fat-iface lookup"
  -- Read .hi directly from disk — bypasses PIT, mi_extra_decls intact.
  ifaceResult <- readExactInterface hscEnv modl
  case ifaceResult of
    Left reason -> do
      case ifaceDbg of
        Just _ -> hPutStrLn stderr $
          "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": could not read .hi file: "
            ++ renderExactInterfaceFailure reason
        Nothing -> pure ()
      return (FatIfaceLoadFailureOutcome (renderExactInterfaceFailure reason))
    Right (iface, _) ->
      case mi_extra_decls iface of
        Nothing -> do
          case ifaceDbg of
            Just _ -> hPutStrLn stderr $
              "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": no mi_extra_decls"
            Nothing -> pure ()
          return FatIfaceNoExtraDeclarations
        Just ifaceBinds -> do
          coreBinds <- initIfaceCheck doc hscEnv $ do
            typeEnvRef <- liftIO $ newIORef emptyTypeEnv
            initIfaceLcl modl doc NotBoot $
              tcTopIfaceBindings typeEnvRef ifaceBinds
          case ifaceDbg of
            Just _ -> hPutStrLn stderr $
              "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": loaded " ++ show (length coreBinds) ++ " bindings"
            Nothing -> pure ()
          return (FatIfaceBindings (IntMap.fromList (zip [0..] coreBinds)) (bindingsToMap coreBinds))

renderExactInterfaceFailure :: ExactInterfaceFailure -> String
renderExactInterfaceFailure failure = case failure of
  ExactInterfaceFinderFailure reason -> reason
  ExactInterfaceReadFailure reason -> reason

-- | Every member of an authoritative Rec group has the same original ordinal.
bindingsToMap :: [CoreBind] -> Map.Map Name Int
bindingsToMap = foldl' addBind Map.empty . zip [0..]
  where
    addBind m (ordinal, NonRec binder _) = Map.insert (varName binder) ordinal m
    addBind m (ordinal, Rec pairs) = foldl' (\m' (binder, _) -> Map.insert (varName binder) ordinal m') m pairs

-- GHC exposes interface-read failures as a closed diagnostic type without an
-- Outputable instance. Keep the reason typed at the lookup boundary while
-- rendering each constructor without dropping its useful context.
renderMissingInterfaceError :: MissingInterfaceError -> String
renderMissingInterfaceError failure = case failure of
  BadSourceImport modl -> "bad source import: " ++ showSDocUnsafe (ppr modl)
  HomeModError _ location -> "home interface error at " ++ show location
  DynamicHashMismatchError modl location ->
    "dynamic hash mismatch for " ++ showSDocUnsafe (ppr modl) ++ " at " ++ show location
  CantFindErr{} -> "interface not found"
  BadIfaceFile readFailure -> "bad interface file: " ++ renderReadInterfaceError readFailure
  FailedToLoadDynamicInterface modl readFailure ->
    "failed to load dynamic interface for " ++ showSDocUnsafe (ppr modl)
      ++ ": " ++ renderReadInterfaceError readFailure

renderReadInterfaceError :: ReadInterfaceError -> String
renderReadInterfaceError failure = case failure of
  ExceptionOccurred path exception -> path ++ ": " ++ displayException exception
  HiModuleNameMismatchWarn path expected actual ->
    path ++ ": expected " ++ showSDocUnsafe (ppr expected)
      ++ ", found " ++ showSDocUnsafe (ppr actual)

-- | The defining context needed after an owner's interface has been read and
-- typechecked. Entry metadata comes from its declarations, independently of
-- the Ids reconstructed by the optional fat Core decoder.
data OwnerInterfaceContext = OwnerInterfaceContext
  { ownerInterfaceLocation :: ModLocation
  , ownerInterfaceTyCons :: [TyCon]
  , ownerInterfaceEntries :: [Id]
  }

-- | Daemon-lifetime cache of an owner module's already-read-and-typechecked
-- defining context: the 'ModLocation' 'readExactInterface' resolved it at,
-- and the type constructors and defining entry Ids 'typecheckIface' produced. Recovered-body
-- preparation ('Tidepool.PreparedStg.prepareRecoveredBodies') reads and
-- typechecks an owner's interface at most once per cache lifetime; only
-- successful outcomes are cached; a failure is retried on the next lookup
-- rather than pinned, since the interface read may simply not have been
-- attempted with the right toolchain state yet.
newtype OwnerInterfaceCache =
  OwnerInterfaceCache (MVar (Map.Map Module OwnerInterfaceContext))

newOwnerInterfaceCache :: IO OwnerInterfaceCache
newOwnerInterfaceCache = OwnerInterfaceCache <$> newMVar Map.empty

-- | Copy the already-read owner contexts into an independent cache.
copyOwnerInterfaceCache :: OwnerInterfaceCache -> IO OwnerInterfaceCache
copyOwnerInterfaceCache (OwnerInterfaceCache cacheRef) =
  OwnerInterfaceCache <$> (readMVar cacheRef >>= newMVar)

-- | Completed defining contexts are selected before their maps are combined.
-- Earlier sources win a selected duplicate owner.
mergeOwnerInterfaceCaches :: [(OwnerInterfaceCache, Module -> Bool)] -> IO OwnerInterfaceCache
mergeOwnerInterfaceCaches sources = do
  selected <- mapM (\(OwnerInterfaceCache ref, keep) ->
    Map.filterWithKey (\owner _ -> keep owner) <$> readMVar ref) sources
  OwnerInterfaceCache <$> newMVar (Map.unions selected)

selectOwnerInterfaceCaches :: [(OwnerInterfaceCache, Set.Set Module)] -> IO OwnerInterfaceCache
selectOwnerInterfaceCaches sources = do
  selected <- mapM (\(OwnerInterfaceCache ref, owners) -> do
    entries <- readMVar ref
    pure (Map.fromAscList [(owner, value) | owner <- Set.toAscList owners
      , Just value <- [Map.lookup owner entries]])) sources
  OwnerInterfaceCache <$> newMVar (Map.unions selected)

lookupOwnerInterface :: OwnerInterfaceCache -> Module
  -> IO (Maybe OwnerInterfaceContext)
lookupOwnerInterface (OwnerInterfaceCache cacheRef) owner =
  Map.lookup owner <$> readMVar cacheRef

cacheOwnerInterface :: OwnerInterfaceCache -> Module
  -> OwnerInterfaceContext -> IO ()
cacheOwnerInterface (OwnerInterfaceCache cacheRef) owner value =
  modifyMVar_ cacheRef (pure . Map.insert owner value)

-- | Drop every cached owner whose 'Module' key matches the given predicate.
-- See 'evictFatIfaceMatching': the same request-boundary invalidation
-- applies here, for the same reason (a target or @Tidepool.Session.*@
-- module's @.hi@ file can change between requests).
evictOwnerInterfaceMatching :: OwnerInterfaceCache -> (Module -> Bool) -> IO ()
evictOwnerInterfaceMatching (OwnerInterfaceCache cacheRef) stale =
  modifyMVar_ cacheRef (pure . Map.filterWithKey (\owner _ -> not (stale owner)))
