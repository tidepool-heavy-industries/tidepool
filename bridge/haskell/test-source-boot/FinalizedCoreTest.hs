module FinalizedCoreTest (finalizedCoreChecks) where

import Control.Exception (bracket)
import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA256
import Data.ByteString qualified as BS
import GHC
import GHC.Core (Bind(..), bindersOfBinds)
import GHC.Core.InstEnv (is_dfun)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCon (tyConName)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Driver.Main (hscTidy)
import GHC.Driver.Session (targetProfile, updOptLevel)
import GHC.Fingerprint.Type (Fingerprint(..))
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Iface.Make (mkIfaceTc)
import GHC.Types.Id (idType)
import GHC.Types.Name (getOccString, isExternalName)
import GHC.Types.SptEntry (SptEntry(..))
import GHC.Types.TyThing (TyThing(..))
import GHC.Types.TypeEnv (lookupTypeEnv)
import GHC.Types.Var (varName)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), emptyHomeModInfoLinkable, lookupHpt)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Unit.Module.ModIface
  ( ModIfaceBackend(..), mi_final_exts, mi_iface_hash, set_mi_extra_decls
  , set_mi_final_exts, set_mi_module )
import GHC.Unit.Types (moduleUnit, unitString)
import GHC.Driver.Env (hsc_HPT, hsc_dflags)
import GHC.ForeignSrcLang (ForeignSrcLang(..))
import Numeric (showHex)
import System.Directory
  ( copyFile, createDirectory, doesFileExist, getTemporaryDirectory
  , removeDirectoryRecursive, removeFile )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), readExactIfaceArtifacts, hydrateExactScope )
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.FinalizedCore
import Tidepool.FinalizedModule (FinalizedModule(..))
import Tidepool.PreparedStg (PreparedModule(..), prepareModule, unelaboratedModule)

-- | A new GHC session hydrates captured interface bytes after the only source
-- file has been removed. Its original Names, polymorphic types, recursive
-- groups, local class instance and CorePrep input must all survive.
finalizedCoreChecks :: IO ()
finalizedCoreChecks = bracket scratch removeDirectoryRecursive $ \work -> do
  let source = work </> "FinalizedCoreFixture.hs"
      interfacePath = work </> "captured-skinny.hi"
  copyFile "test-source-boot/fixtures/FinalizedCoreFixture.hs" source
  libdir <- getLibdir
  (artifact, bytes, summary, groups, tyconNames, instanceNames) <- runGhc (Just libdir) $ do
    configure work
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    _ <- depanal [] False
    summary <- getModSummary (mkModuleName "FinalizedCoreFixture")
    parsed <- parseModule summary
    typed <- typecheckModule parsed
    desugared <- desugarModule typed
    env <- getSession
    simplified <- liftIO (core2core env (dm_core_module desugared))
    (guts, details) <- liftIO (hscTidy env simplified)
    let (tcg, _) = tm_internals_ typed
    interface <- liftIO (mkIfaceTc env Sf_None details summary (Just (cg_binds guts)) tcg)
    let home = HomeModInfo (set_mi_extra_decls Nothing interface) details emptyHomeModInfoLinkable
        finalized = FinalizedModule home guts
    liftIO $ do
      bytes <- requireRight =<< captureFinalizedCore env finalized work
      forM_ (take 1 (bindersOfBinds (cg_binds guts))) $ \binder -> do
        refusal <- captureFinalizedCore env
          finalized { finalizedTidyGuts = guts
            { cg_spt_entries = [SptEntry binder (Fingerprint 1 2)] } } work
        assert (case refusal of Left (FinalizedCoreUnsupported StaticPointerEntries) -> True; _ -> False)
          "static-pointer metadata was discarded instead of explicitly refused"
      refusal <- captureFinalizedCore env
        finalized { finalizedTidyGuts = guts
          { cg_foreign_files = [(LangC, work </> "unavailable.c")] } } work
      assert (case refusal of Left (FinalizedCoreUnsupported ForeignSourceFiles) -> True; _ -> False)
        "foreign source metadata was discarded instead of explicitly refused"
      writeBinIface (targetProfile (hsc_dflags env)) QuietBinIFace NormalCompression
        interfacePath (hm_iface home)
      interfaceBytes <- BS.readFile interfacePath
      let owner = cg_module guts
          artifact = ExactIfaceArtifact (unitString (moduleUnit owner))
            "FinalizedCoreFixture" interfacePath (hexBytes (SHA256.hash interfaceBytes)) []
      pure (artifact, bytes, summary {ms_hspp_buf = Nothing}, bindingGroups guts,
        map (getOccString . tyConName) (cg_tycons guts),
        map (getOccString . is_dfun) (finalizedCoreSiteInstances finalized))
  removeFile source
  exists <- doesFileExist source
  assert (not exists) "source-free roundtrip kept its source"
  runGhc (Just libdir) $ do
    configure work
    empty <- getSession
    interfaces <- liftIO (requireRight =<< readExactIfaceArtifacts empty [artifact])
    env <- liftIO (hydrateExactScope empty interfaces)
    setSession env
    home <- case lookupHpt (hsc_HPT env) (mkModuleName "FinalizedCoreFixture") of
      Just home -> pure home
      Nothing -> fail "cold exact HPT has no original owner"
    liftIO $ do
      finalized <- requireRight =<< decodeFinalizedCore env home (ms_location summary) bytes
      let guts = finalizedTidyGuts finalized
      assert (bindingGroups guts == groups) "canonical binding order or recursive groups changed"
      assert (any (\(recursive, names) -> recursive
        && any (\name -> name == "evenBox" || name == "oddBox" || name == "$wevenBox" || name == "$woddBox") names) groups)
        "fixture failed to preserve its native recursive binding group"
      assert (any (\(_, names) -> any (\name -> name == "privateHelper" || name == "$wprivateHelper") names) groups)
        "fixture lost the non-exported private helper before capture"
      assert (map (getOccString . tyConName) (cg_tycons guts) == tyconNames)
        "canonical TyCon order changed"
      assert (not (null instanceNames) &&
        map (getOccString . is_dfun) (finalizedCoreSiteInstances finalized) == instanceNames)
        "cold canonical site authority lost its local class instance"
      forM_ (bindersOfBinds (cg_binds guts)) $ \binder ->
        if not (isExternalName (varName binder)) then pure () else
          case lookupTypeEnv (md_types (hm_details home)) (varName binder) of
            Just (AnId original) -> assert (varName original == varName binder
              && eqType (idType original) (idType binder))
                "cold Core rebound an original native Name or type"
            _ -> fail "cold Core binder has no admitted original declaration"
      prepared <- prepareModule env summary (unelaboratedModule guts)
      assert (not (null (pmBindings prepared))) "cold canonical Core produced no STG"
      let owner = cg_module guts
          wrongHome = home { hm_iface = set_mi_module
            (mkModule (moduleUnit owner) (mkModuleName "OtherOriginal")) (hm_iface home) }
      wrongOwner <- decodeFinalizedCore env wrongHome (ms_location summary) bytes
      assert (case wrongOwner of Left FinalizedCoreOwnerMismatch -> True; _ -> False)
        "canonical companion accepted a different exact owner"
      let backend = mi_final_exts (hm_iface home)
          fingerprint = mi_iface_hash backend
          different = if fingerprint == Fingerprint 0 0 then Fingerprint 1 0 else Fingerprint 0 0
          wrongVersion = home { hm_iface = set_mi_final_exts
            backend {mi_iface_hash = different} (hm_iface home) }
      mismatch <- decodeFinalizedCore env wrongVersion (ms_location summary) bytes
      assert (case mismatch of Left FinalizedCoreInterfaceMismatch -> True; _ -> False)
        "canonical companion accepted a different native interface version"
      malformed <- decodeFinalizedCore env home (ms_location summary) (BS.take 1 bytes)
      assert (case malformed of Left FinalizedCoreDecodeFailure{} -> True; _ -> False)
        "truncated canonical companion did not fail closed"
  putStrLn "finalized Core: cold source-free STG, native owner/types, Rec groups and local instances; owner/version/truncation and unsupported metadata checked"
  where
    configure work = do
      flags <- getSessionDynFlags
      _ <- setSessionDynFlags (updOptLevel 2 flags)
        { backend = noBackend, ghcLink = NoLink, importPaths = [work]
        , hiDir = Just work, objectDir = Just work }
      pure ()
    scratch = do
      root <- getTemporaryDirectory
      (path, handle) <- openTempFile root "tidepool-finalized-core"
      hClose handle
      removeFile path
      createDirectory path
      pure path

bindingGroups :: CgGuts -> [(Bool, [String])]
bindingGroups = map group . cg_binds
  where
    group (NonRec binder _) = (False, [getOccString binder])
    group (Rec pairs) = (True, map (getOccString . fst) pairs)

requireRight :: Show failure => Either failure value -> IO value
requireRight = either (fail . show) pure

assert :: Bool -> String -> IO ()
assert condition message = unless condition (fail message)

hexBytes :: BS.ByteString -> String
hexBytes = concatMap (\byte -> let rendered = showHex byte ""
  in replicate (2 - length rendered) '0' ++ rendered) . BS.unpack
