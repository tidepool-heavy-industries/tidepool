-- | Extract Core bindings from "fat" interface files (.hi compiled with
-- -fwrite-if-simplified-core). These contain mi_extra_decls: the full
-- post-optimization Core for ALL bindings including workers, loop-breakers,
-- and internal helpers that don't get normal unfoldings.
--
-- Uses findAndReadIface to read .hi files directly from disk, bypassing the
-- PIT (Package Interface Table) cache. The PIT replaces mi_extra_decls with
-- a panic thunk to save memory, so loadSysInterface can't be used here.
module Tidepool.FatIface
  ( FatIfaceCache, newFatIfaceCache, lookupFatIface
  , FatIfaceLookup(..), FatIfaceMissing(..), lookupFatIfaceExact
  , ExactInterfaceFailure(..), readExactInterface
  ) where

import GHC.Core (CoreBind, Bind(..))
import GHC.Driver.Env (HscEnv, hsc_NC, hsc_dflags)
import GHC.Types.Name (Name, nameModule_maybe)
import GHC.Types.Var (varName)
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
import GHC.Unit.Module.Location (ModLocation(ml_hi_file))

import Control.Concurrent.MVar (MVar, modifyMVar, newMVar)
import Control.Exception
  ( SomeAsyncException, SomeException, displayException, fromException, throwIO, try )
import Control.Monad.IO.Class (liftIO)
import Data.IORef (newIORef)
import qualified Data.Map.Strict as Map
import System.IO (hPutStrLn, stderr)
import System.Environment (lookupEnv)

-- | Exact recovery keeps absence distinct from an unreadable interface.
-- The legacy Maybe view must not be used by prepared body recovery.
data FatIfaceMissing = NameWithoutModule | NoExtraDeclarations | BindingAbsent
  deriving (Eq, Show)

data FatIfaceLookup
  = FatIfaceFound CoreBind
  | FatIfaceMissing FatIfaceMissing
  | FatIfaceLoadFailure Module String

-- | An interface load is cached as an outcome, not as a map.  In particular,
-- an unreadable interface and an interface without extra declarations must not
-- become indistinguishable from a successfully loaded interface with no
-- matching binding.
data FatIfaceModule
  = FatIfaceBindings (Map.Map Name CoreBind)
  | FatIfaceNoExtraDeclarations
  | FatIfaceLoadFailureOutcome String

data ExactInterfaceFailure
  = ExactInterfaceFinderFailure String
  | ExactInterfaceReadFailure String
  deriving (Eq, Show)

lookupFatIfaceExact :: HscEnv -> FatIfaceCache -> Name -> IO FatIfaceLookup
lookupFatIfaceExact hscEnv cache name = case nameModule_maybe name of
  Nothing -> pure (FatIfaceMissing NameWithoutModule)
  Just modl -> do
    outcome <- lookupModuleOutcome hscEnv cache modl
    pure $ case outcome of
      FatIfaceBindings nameMap -> case Map.lookup name nameMap of
        Just bind -> FatIfaceFound bind
        Nothing -> FatIfaceMissing BindingAbsent
      FatIfaceNoExtraDeclarations -> FatIfaceMissing NoExtraDeclarations
      FatIfaceLoadFailureOutcome reason -> FatIfaceLoadFailure modl reason

-- | Cache of deserialized fat interface Core, keyed by Module.
-- Each module's extra-decls are deserialized at most once.
-- For each Name, we store the full CoreBind it belongs to — this preserves
-- Rec group structure so that looking up any member returns all siblings
-- (critical for join points that reference each other within a Rec group).
newtype FatIfaceCache = FatIfaceCache (MVar (Map.Map Module FatIfaceModule))

-- | Create an empty cache.
newFatIfaceCache :: IO FatIfaceCache
newFatIfaceCache = FatIfaceCache <$> newMVar Map.empty

-- | Look up a Name's CoreBind from the fat interface of its defining module.
-- For NonRec bindings, returns the single binding.
-- For Rec bindings, returns the FULL Rec group — this is critical because
-- Rec groups may contain join points that siblings reference. Without the
-- full group, join point definitions are lost and the JIT emits
-- "Jump to unknown label JoinId(...)".
--
-- Returns Nothing if:
--   - The Name has no module (local/anonymous)
--   - The module wasn't compiled with -fwrite-if-simplified-core
--   - The binding isn't found in mi_extra_decls
lookupFatIface :: HscEnv -> FatIfaceCache -> Name -> IO (Maybe CoreBind)
lookupFatIface hscEnv (FatIfaceCache cacheRef) name = do
  case nameModule_maybe name of
    Nothing -> return Nothing
    Just modl -> do
      outcome <- lookupModuleOutcome hscEnv (FatIfaceCache cacheRef) modl
      case outcome of
        FatIfaceBindings nameMap -> return (Map.lookup name nameMap)
        FatIfaceNoExtraDeclarations -> return Nothing
        FatIfaceLoadFailureOutcome _ -> return Nothing

-- | Load one module once and retain whether it loaded, lacked extra
-- declarations, or failed.  Holding the MVar across the miss path also keeps
-- the "at most once" cache invariant true when resolution is concurrent.
lookupModuleOutcome :: HscEnv -> FatIfaceCache -> Module -> IO FatIfaceModule
lookupModuleOutcome hscEnv (FatIfaceCache cacheRef) modl =
  modifyMVar cacheRef $ \cache -> case Map.lookup modl cache of
    Just outcome -> pure (cache, outcome)
    Nothing -> do
      outcome <- loadModuleExtraDecls hscEnv modl
      pure (Map.insert modl outcome cache, outcome)

-- | Load and deserialize mi_extra_decls for a single module, retaining the
-- exact outcome for both exact and legacy callers.
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

-- | Catch ordinary interface failures while allowing asynchronous exceptions
-- (notably cancellation) to escape the cache loader.
trySynchronous :: IO a -> IO (Either SomeException a)
trySynchronous action = do
  result <- try action
  case result of
    Left e -> case (fromException e :: Maybe SomeAsyncException) of
      Just async -> throwIO async
      Nothing -> pure (Left e)
    Right value -> pure (Right value)

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
          return (FatIfaceBindings (bindingsToMap coreBinds))

renderExactInterfaceFailure :: ExactInterfaceFailure -> String
renderExactInterfaceFailure failure = case failure of
  ExactInterfaceFinderFailure reason -> reason
  ExactInterfaceReadFailure reason -> reason

-- | Index CoreBinds into a Name→CoreBind map.
-- For NonRec bindings, each name maps to its own NonRec.
-- For Rec bindings, EVERY member maps to the SAME full Rec group.
-- This preserves Rec group structure for join point resolution.
bindingsToMap :: [CoreBind] -> Map.Map Name CoreBind
bindingsToMap = foldl' addBind Map.empty
  where
    addBind m bind@(NonRec b _) = Map.insert (varName b) bind m
    addBind m bind@(Rec pairs)  = foldl' (\m' (b, _) -> Map.insert (varName b) bind m') m pairs

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
