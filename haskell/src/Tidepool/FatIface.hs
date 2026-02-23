-- | Extract Core bindings from "fat" interface files (.hi compiled with
-- -fwrite-if-simplified-core). These contain mi_extra_decls: the full
-- post-optimization Core for ALL bindings including workers, loop-breakers,
-- and internal helpers that don't get normal unfoldings.
--
-- Uses findAndReadIface to read .hi files directly from disk, bypassing the
-- PIT (Package Interface Table) cache. The PIT replaces mi_extra_decls with
-- a panic thunk to save memory, so loadSysInterface can't be used here.
module Tidepool.FatIface (FatIfaceCache, newFatIfaceCache, lookupFatIface) where

import GHC.Core (CoreBind, CoreExpr, Bind(..))
import GHC.Driver.Env (HscEnv)
import GHC.Types.Name (Name, nameModule_maybe)
import GHC.Types.Var (varName)
import GHC.Unit.Types (Module, moduleUnit, moduleName, mkModule, toUnitId)
import GHC.Unit.Module.ModIface (mi_extra_decls)
import GHC.Utils.Outputable (showSDocUnsafe, ppr, text)

import GHC.Iface.Load (findAndReadIface)
import GHC.IfaceToCore (tcTopIfaceBindings)
import GHC.Tc.Utils.Monad (initIfaceCheck, initIfaceLcl)
import GHC.Types.TypeEnv (emptyTypeEnv)
import GHC.Data.Maybe (MaybeErr(..))
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))

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
-- Uses findAndReadIface to bypass the PIT cache (which strips mi_extra_decls).
loadModuleExtraDecls :: HscEnv -> Module -> IO (Map.Map Name CoreExpr)
loadModuleExtraDecls hscEnv modl = do
  result <- try $ loadModuleExtraDeclsUnsafe hscEnv modl
  case result of
    Right m -> return m
    Left (e :: SomeException) -> do
      hPutStrLn stderr $
        "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": exception: " ++ show e
      return Map.empty

loadModuleExtraDeclsUnsafe :: HscEnv -> Module -> IO (Map.Map Name CoreExpr)
loadModuleExtraDeclsUnsafe hscEnv modl = do
  let doc = text "tidepool fat-iface lookup"
      -- findAndReadIface wants InstalledModule (GenModule UnitId)
      installedMod = mkModule (toUnitId (moduleUnit modl)) (moduleName modl)
  -- Read .hi directly from disk — bypasses PIT, mi_extra_decls intact
  readResult <- findAndReadIface hscEnv doc installedMod modl NotBoot
  case readResult of
    Failed _err -> do
      hPutStrLn stderr $
        "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": could not read .hi file"
      return Map.empty
    Succeeded (iface, _loc) ->
      case mi_extra_decls iface of
        Nothing -> do
          hPutStrLn stderr $
            "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": no mi_extra_decls"
          return Map.empty
        Just ifaceBinds -> do
          coreBinds <- initIfaceCheck doc hscEnv $ do
            typeEnvRef <- liftIO $ newIORef emptyTypeEnv
            initIfaceLcl modl doc NotBoot $
              tcTopIfaceBindings typeEnvRef ifaceBinds
          hPutStrLn stderr $
            "  [fat-iface] " ++ showSDocUnsafe (ppr modl) ++ ": loaded " ++ show (length coreBinds) ++ " bindings"
          return (bindingsToMap coreBinds)

-- | Flatten CoreBinds into a Name→CoreExpr map.
bindingsToMap :: [CoreBind] -> Map.Map Name CoreExpr
bindingsToMap = foldl' addBind Map.empty
  where
    addBind m (NonRec b e) = Map.insert (varName b) e m
    addBind m (Rec pairs)  = foldl' (\m' (b, e) -> Map.insert (varName b) e m') m pairs
