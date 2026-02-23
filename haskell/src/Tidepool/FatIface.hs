-- | Extract Core bindings from "fat" interface files (.hi compiled with
-- -fwrite-if-simplified-core). These contain mi_extra_decls: the full
-- post-optimization Core for ALL bindings including workers, loop-breakers,
-- and internal helpers that don't get normal unfoldings.
--
-- Used as a fallback when realIdUnfolding returns NoUnfolding.
module Tidepool.FatIface (FatIfaceCache, newFatIfaceCache, lookupFatIface) where

import GHC.Core (CoreBind, CoreExpr, Bind(..))
import GHC.Driver.Env (HscEnv)
import GHC.Types.Name (Name, nameModule_maybe)
import GHC.Types.Var (varName)
import GHC.Unit.Types (Module)
import GHC.Utils.Outputable (showSDocUnsafe, ppr)
import GHC.Unit.Module.ModIface (mi_extra_decls)

import GHC.Iface.Load (loadSysInterface)
import GHC.IfaceToCore (tcTopIfaceBindings)
import GHC.Tc.Utils.Monad (initIfaceCheck, initIfaceLcl)
import GHC.Types.TypeEnv (emptyTypeEnv)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Utils.Outputable (text)

import Control.Exception (SomeException, try)
import Control.Monad.IO.Class (liftIO)
import Data.Foldable (foldl')
import Data.IORef (IORef, newIORef, readIORef, modifyIORef')
import qualified Data.Map.Strict as Map
import System.IO (hPutStrLn, stderr)

-- | Cache of deserialized fat interface Core, keyed by Module.
-- Each module's extra-decls are deserialized at most once.
newtype FatIfaceCache = FatIfaceCache (IORef (Map.Map Module (Map.Map Name CoreExpr)))

-- | Create an empty cache.
newFatIfaceCache :: IO FatIfaceCache
newFatIfaceCache = FatIfaceCache <$> newIORef Map.empty

-- | Look up a Name's CoreExpr from the fat interface of its defining module.
-- Returns Nothing if:
--   - The Name has no module (local/anonymous)
--   - The module wasn't compiled with -fwrite-if-simplified-core
--   - The binding isn't found in mi_extra_decls
lookupFatIface :: HscEnv -> FatIfaceCache -> Name -> IO (Maybe CoreExpr)
lookupFatIface hscEnv (FatIfaceCache cacheRef) name = do
  case nameModule_maybe name of
    Nothing -> return Nothing
    Just modl -> do
      cache <- readIORef cacheRef
      nameMap <- case Map.lookup modl cache of
        Just m -> return m
        Nothing -> do
          m <- loadModuleExtraDecls hscEnv modl
          modifyIORef' cacheRef (Map.insert modl m)
          return m
      return (Map.lookup name nameMap)

-- | Load and deserialize mi_extra_decls for a single module.
-- Returns an empty map if the module has no fat interface or if deserialization
-- fails (e.g., GHC panics with "No mi_extra_decls in PIT" when a transitive
-- dependency wasn't compiled with -fwrite-if-simplified-core).
loadModuleExtraDecls :: HscEnv -> Module -> IO (Map.Map Name CoreExpr)
loadModuleExtraDecls hscEnv modl = do
  result <- try $ loadModuleExtraDeclsUnsafe hscEnv modl
  case result of
    Right m -> return m
    Left (e :: SomeException) -> do
      hPutStrLn stderr $
        "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": exception during tcTopIfaceBindings: " ++ show e
      return Map.empty

loadModuleExtraDeclsUnsafe :: HscEnv -> Module -> IO (Map.Map Name CoreExpr)
loadModuleExtraDeclsUnsafe hscEnv modl = do
  let doc = text "tidepool fat-iface lookup"
  coreBinds <- initIfaceCheck doc hscEnv $ do
    iface <- loadSysInterface doc modl
    case mi_extra_decls iface of
      Nothing -> do
        liftIO $ hPutStrLn stderr $
          "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": no mi_extra_decls (missing -fwrite-if-simplified-core?)"
        return []
      Just ifaceBinds -> do
        typeEnvRef <- liftIO $ newIORef emptyTypeEnv
        binds <- initIfaceLcl modl doc NotBoot $
          tcTopIfaceBindings typeEnvRef ifaceBinds
        liftIO $ hPutStrLn stderr $
          "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": loaded " ++ show (length binds) ++ " bindings"
        return binds
  return (bindingsToMap coreBinds)

-- | Flatten CoreBinds into a Name→CoreExpr map.
bindingsToMap :: [CoreBind] -> Map.Map Name CoreExpr
bindingsToMap = foldl' addBind Map.empty
  where
    addBind m (NonRec b e) = Map.insert (varName b) e m
    addBind m (Rec pairs)  = foldl' (\m' (b, e) -> Map.insert (varName b) e m') m pairs
