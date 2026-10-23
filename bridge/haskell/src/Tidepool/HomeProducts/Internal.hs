-- | Physical interface views borrowed by one active compiler cycle. Captured
-- TYPE owners remain independent of these disposable filesystem adapters.
module Tidepool.HomeProducts.Internal
  ( CycleInterfaceViews, withCompilerInterfaceViews, compilerInterfaceDirectory
  , BackedInterface, materializeCapturedInterfaceView, backedInterfaceLocation
  , registerBackedInterface
  ) where

import Control.Exception (bracket, finally, evaluate)
import Control.Monad (unless)
import Data.IORef (IORef, newIORef, readIORef, writeIORef)
import qualified Data.ByteString as BS
import GHC (Ghc)
import GHC.Driver.Env (HscEnv, hsc_FC, hsc_HPT, hsc_home_unit)
import GHC.Driver.Monad (reflectGhc, reifyGhc)
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Unit.Home.ModInfo (hm_iface, lookupHpt)
import GHC.Unit.Module (Module, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_iface_hash, mi_final_exts, mi_hsc_src)
import GHC.Types.SourceFile (HscSource(..))
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Utils.Fingerprint (Fingerprint, fingerprint0)
import System.Mem.StableName (StableName, makeStableName)
import GHC.Unit.Module.Location (ModLocation(..))
import GHC.Unit.Types (GenWithIsBoot(..))
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import System.Directory
  ( doesFileExist, getTemporaryDirectory, removeDirectoryRecursive, removeFile, renameFile )
import System.FilePath ((</>))
import System.IO (hClose, hIsClosed, openBinaryTempFile)
import System.Posix.Temp (mkdtemp)
import Tidepool.ArtifactBytes (ArtifactBytes, artifactBytes, artifactSha256)
import Tidepool.Timing (readTimingEnabled, emitCount)

-- The existing cycle bracket is the only issuer. The live cell fences an
-- escaped adapter; retaining it cannot prolong the bracket or its files.
data CycleInterfaceViews = CycleInterfaceViews FilePath (IORef Bool)
data BackedInterface = BackedInterface CycleInterfaceViews Module String Fingerprint
  HscSource (StableName ModIface) ModLocation

withCompilerInterfaceViews :: (CycleInterfaceViews -> Ghc a) -> Ghc a
withCompilerInterfaceViews action = reifyGhc $ \session -> bracket acquire release
  (\views -> reflectGhc (action views) session)
  where
    acquire = do
      directory <- getTemporaryDirectory >>= \root -> mkdtemp (root </> "tidepool-compiler-view.XXXXXX")
      CycleInterfaceViews directory <$> newIORef True
    release (CycleInterfaceViews directory live) =
      writeIORef live False `finally` removeDirectoryRecursive directory

compilerInterfaceDirectory :: CycleInterfaceViews -> IO FilePath
compilerInterfaceDirectory (CycleInterfaceViews directory live) = do
  active <- readIORef live
  unless active (fail "compiler interface view belongs to a completed cycle")
  pure directory

-- Only the exact-interface admission owner calls this leaf with its retained
-- bytes and decoded Module. Repeated phase environments borrow the same view;
-- an existing output must still contain those exact bytes.
materializeCapturedInterfaceView :: CycleInterfaceViews -> ModIface -> ArtifactBytes -> IO BackedInterface
materializeCapturedInterfaceView views interface body = do
  iface <- evaluate interface
  identity <- makeStableName iface
  let owner = mi_module iface
  directory <- compilerInterfaceDirectory views
  let path = directory </> artifactSha256 body ++ ".hi"
      bytes = artifactBytes body
  exists <- doesFileExist path
  timing <- readTimingEnabled
  if exists then do
    actual <- BS.readFile path
    unless (actual == bytes) (fail "compiler interface view conflicts with captured bytes")
    emitCount timing ("exact_iface_view_reuse." ++ moduleNameString (moduleName owner)) 1
  else do
    bracket (openBinaryTempFile directory "interface-view.hi")
      (\(temporary,handle) -> do
        closed <- hIsClosed handle
        unless closed (hClose handle)
        remaining <- doesFileExist temporary
        if remaining then removeFile temporary else pure ())
      (\(temporary,handle) -> do
        BS.hPut handle bytes
        hClose handle
        renameFile temporary path)
    emitCount timing ("exact_iface_view_write_bytes." ++ moduleNameString (moduleName owner))
      (toInteger (BS.length bytes))
  let location = ModLocation
        { ml_hs_file=Nothing, ml_hi_file=path, ml_dyn_hi_file=path
        , ml_obj_file=path, ml_dyn_obj_file=path, ml_hie_file=path }
  pure (BackedInterface views owner (artifactSha256 body)
    (mi_iface_hash (mi_final_exts iface)) (mi_hsc_src iface) identity location)

backedInterfaceLocation :: Module -> String -> BackedInterface -> IO ModLocation
backedInterfaceLocation owner sha (BackedInterface views actual seal _ _ _ location) = do
  _ <- compilerInterfaceDirectory views
  unless (owner == actual && sha == seal)
    (fail "compiler interface view differs from its selected owner")
  pure location

registerBackedInterface :: HscEnv -> Module -> String -> BackedInterface -> IO ()
registerBackedInterface environment owner sha view@(BackedInterface _ _ _ fingerprint source identity _) = do
  location <- backedInterfaceLocation owner sha view
  unless (moduleUnit owner == homeUnitAsUnit (hsc_home_unit environment))
    (fail "compiler interface view is outside the active HOME unit")
  case lookupHpt (hsc_HPT environment) (moduleName owner) of
    Just home | mi_module (hm_iface home) == owner
        , mi_iface_hash (mi_final_exts (hm_iface home)) == fingerprint
        , mi_hsc_src (hm_iface home) == source ->
      if fingerprint /= fingerprint0 then pure () else do
        actual <- evaluate (hm_iface home) >>= makeStableName
        unless (actual == identity) (fail "thin interface view lost its installed allocation")
    _ -> fail "compiler interface view lacks its installed HOME owner"
  _ <- addHomeModuleToFinder (hsc_FC environment) (hsc_home_unit environment)
    (GWIB (moduleName owner) (if source == HsBootFile then IsBoot else NotBoot)) location
  pure ()
