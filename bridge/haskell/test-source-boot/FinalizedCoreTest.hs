{-# LANGUAGE GADTs #-}

module FinalizedCoreTest (finalizedCoreChecks) where

import Control.Exception (bracket, try)
import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA256
import Data.ByteString qualified as BS
import Data.Dynamic (fromDynamic)
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import GHC
import GHC.Core (Bind(..), bindersOfBinds)
import GHC.Core.InstEnv (is_dfun)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCon (tyConName)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Cmm.CLabel (mkInitializerStubLabel)
import GHC.Data.FastString (fsLit)
import GHC.Types.ForeignStubs (ForeignStubs(..), CHeader(..), CStub(..))
import GHC.Utils.Outputable qualified as Outputable
import GHC.Driver.Main (hscTidy, loadIfaceByteCode)
import GHC.Driver.Make (load')
import GHC.Driver.Pipeline.Execute (runPhase)
import GHC.Driver.Pipeline.Phases (TPhase(..), PhaseHook(..))
import GHC.Driver.Hooks (runPhaseHook)
import GHC.ByteCode.Types (CompiledByteCode(..))
import GHC.Data.FlatBag (elemsFlatBag)
import GHC.Linker.Types (linkableModule, linkableParts, linkablePartAllBCOs)
import GHC.Driver.Session (targetProfile, updOptLevel)
import GHC.Fingerprint.Type (Fingerprint(..))
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Iface.Make (mkIfaceTc)
import GHC.Types.Name (getOccString)
import GHC.Types.SptEntry (SptEntry(..))
import GHC.Types.TypeEnv (lookupTypeEnv)
import GHC.Types.Var (varName)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), HomeModLinkable(..), emptyHomeModInfoLinkable, justBytecode, addToHpt, lookupHpt)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Unit.Module.ModIface
  ( ModIfaceBackend(..), mi_iface_hash, set_mi_extra_decls
  , set_mi_usages, set_mi_final_exts, set_mi_module )
import GHC.Unit.Module.Deps (Usage(..))
import GHC.Unit.Module.Graph (ModuleGraphNode(..))
import GHC.Unit.Types (unitString, unitIdString, toUnitId, stringToUnit, GenWithIsBoot(..))
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Driver.Env (HscEnv(..), hsc_HPT, hscUpdateHPT, hsc_dflags, hsc_all_home_unit_ids, hsc_home_unit)
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.ForeignSrcLang (ForeignSrcLang(..))
import Numeric (showHex)
import System.Directory
  ( copyFile, createDirectory, doesFileExist, getTemporaryDirectory
  , removeDirectoryRecursive, removeFile )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), readExactIfaceArtifacts, hydrateExactScope
  , newOriginalInterfaceArtifacts )
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.FinalizedCore
import Tidepool.FinalizedModule (FinalizedModule(..))
import Tidepool.ExactScope
  ( CanonicalInterfaceProof, CanonicalInterfaceAdmission(..), admittedInterfaceCore
  , validateCandidateCanonicalInterfaceProof )
import Tidepool.FinalizedModuleArtifacts
  ( LocalFinalizedAdmission, captureFinalizedModuleArtifacts, finalizedLocalAdmissions )
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), ProductAvailability(..), sourceEvidence )
import Tidepool.ModuleCandidates (readModuleCandidates)
import Tidepool.PackageWitness (emptyPackageImports, encodePackageImports)
import Tidepool.HomeProducts
  ( CandidateCoreFailure(..), materializeCandidateCompilerView
  , admittedCompilerInterface, validateAdmittedInterfaceRequirements
  , validateCandidateInterfaceRequirements )
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
  (artifact, bytes, summary, proof, localProof, groups, tyconNames, instanceNames, packages) <- runGhc (Just libdir) $ do
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
      let emptyStubs = ForeignStubs (CHeader Outputable.empty) (CStub Outputable.empty [] [])
          initializer = mkInitializerStubLabel (cg_module guts) (fsLit "canonical_test_init")
          nonemptyStubs =
            [ForeignStubs (CHeader (Outputable.text "extern void canonical_test(void);")) (CStub Outputable.empty [] [])
            ,ForeignStubs (CHeader Outputable.empty) (CStub (Outputable.text "void canonical_test(void) {}") [] [])
            ,ForeignStubs (CHeader Outputable.empty) (CStub Outputable.empty [initializer] [])
            ,ForeignStubs (CHeader Outputable.empty) (CStub Outputable.empty [] [initializer])]
      emptyCapture <- requireRight =<< captureFinalizedCore env
        finalized {finalizedTidyGuts = guts {cg_foreign = emptyStubs}} work
      assert (emptyCapture == bytes) "semantically empty GHC foreign stubs changed canonical Core"
      forM_ nonemptyStubs $ \stubs -> do
        refusal <- captureFinalizedCore env
          finalized {finalizedTidyGuts = guts {cg_foreign = stubs}} work
        assert (case refusal of Left (FinalizedCoreUnsupported ForeignExportStubs) -> True; _ -> False)
          "nonempty foreign header/body/initializer/finalizer was discarded"
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
      proof <- captureProof env artifact source bytes work
      localProof <- captureLocalProof env finalized source work
      pure (artifact, bytes, summary {ms_hspp_buf = Nothing}, proof, localProof, bindingGroups guts,
        map (getOccString . tyConName) (cg_tycons guts),
        map (getOccString . is_dfun) (finalizedCoreSiteInstances finalized), cg_dep_pkgs guts)
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
      let missing = mkModuleName "MissingCanonicalDependency"
          homeUsage = UsageHomeModuleInterface missing (toUnitId (moduleUnit (ms_mod summary))) (Fingerprint 0 0)
          packageUsage = UsagePackageModule (mkModule (moduleUnit (ms_mod summary)) missing) (Fingerprint 0 0) False
      forM_ [homeUsage,packageUsage] $ \usage ->
        assert (validateCandidateInterfaceRequirements proof
          (set_mi_usages (usage : mi_usages (hm_iface home)) (hm_iface home))
            == Left CandidateInterfaceRequirementsMismatch)
          "native home interface usage escaped canonical requirement checks"
      forM_ [DurableInterfaceAdmission proof, LocalInterfaceAdmission localProof] $ \admission -> do
        assert (validateAdmittedInterfaceRequirements admission (hm_iface home) == Right ())
          "admitted interface lost its original home dependency inventory"
        forM_ [homeUsage,packageUsage] $ \usage ->
          assert (validateAdmittedInterfaceRequirements admission
            (set_mi_usages (usage : mi_usages (hm_iface home)) (hm_iface home))
              == Left CandidateInterfaceRequirementsMismatch)
            "admitted interface usage escaped canonical requirement checks"
        attached <- admittedCompilerInterface env admission (ms_mod summary)
        assert (mi_module attached == ms_mod summary && case mi_extra_decls attached of
            Just _ -> True; Nothing -> False)
          "admitted executable interface lost its original owner or defining Core"
        wrongOwner <- try (admittedCompilerInterface env admission
          (mkModule (stringToUnit "another-home-unit") (ms_mod_name summary)))
          :: IO (Either CandidateCoreFailure ModIface)
        assert (case wrongOwner of Left CandidateCoreHomeMissing -> True; _ -> False)
          "admitted executable interface accepted a different exact unit"
      (localCorePath,_) <- maybe (fail "local capture lost its canonical Core") pure
        (admittedInterfaceCore (LocalInterfaceAdmission localProof))
      localBytes <- BS.readFile localCorePath
      bracket (BS.writeFile localCorePath (BS.take 1 localBytes))
        (\_ -> BS.writeFile localCorePath localBytes) $ \_ -> do
          changed <- try (admittedCompilerInterface env (LocalInterfaceAdmission localProof)
            (ms_mod summary)) :: IO (Either CandidateCoreFailure ModIface)
          assert (case changed of Left CandidateCoreBytesMismatch -> True; _ -> False)
            "local admitted executable interface accepted substituted Core"
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
      assert (cg_dep_pkgs guts == packages) "canonical native package dependencies changed"
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
  directBytecodeChecks libdir work artifact summary (LocalInterfaceAdmission localProof)
  makeViewChecks libdir work artifact bytes summary proof
  putStrLn "finalized Core: executed one case; source-free STG, direct bytecode and GHC make execution with zero module frontends; staged type-only/Core views, durable interface, native requirements, Core seal and unsupported metadata checked"
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

-- One cold compiler consumes the same captured local original through GHC's
-- supported bytecode API, without a make node or another fixture compilation.
directBytecodeChecks
  :: FilePath -> FilePath -> ExactIfaceArtifact -> ModSummary
  -> CanonicalInterfaceAdmission -> IO ()
directBytecodeChecks libdir work artifact summary proof = runGhc (Just libdir) $ do
  _ <- setSessionDynFlags (ms_hspp_opts summary)
    { backend = interpreterBackend, ghcLink = LinkInMemory
    , importPaths = [work], hiDir = Just work, objectDir = Just work }
  fresh <- getSession
  interfaces <- liftIO (requireRight =<< readExactIfaceArtifacts fresh [artifact])
  admitted <- liftIO (hydrateExactScope fresh interfaces)
  home <- maybe (fail "direct bytecode has no original home owner") pure
    (lookupHpt (hsc_HPT admitted) (ms_mod_name summary))
  frontends <- liftIO (newIORef (0 :: Int))
  let hook :: TPhase result -> IO result
      hook phase@T_Hsc{} = do
        modifyIORef' frontends (+1)
        runPhase phase
      hook phase = runPhase phase
      location = (ms_location summary) {ml_hs_file = Nothing}
      observed = admitted {hsc_hooks = (hsc_hooks admitted)
        {runPhaseHook = Just (PhaseHook hook)}}
      selectedSummary = summary {ms_location = location, ms_hspp_opts = hsc_dflags observed}
  attached <- liftIO (admittedCompilerInterface observed proof (ms_mod summary))
  compile <- maybe (fail "admitted Core has no supported bytecode compiler") pure
    (loadIfaceByteCode observed attached location (md_types (hm_details home)))
  bytecode <- liftIO compile
  let executable = home {hm_linkable = justBytecode bytecode}
      ready = (hscUpdateHPT (\table -> addToHpt table (ms_mod_name summary) executable) observed)
        {hsc_mod_graph = mkModuleGraph [ModuleNode [] selectedSummary]}
  setSession ready
  _ <- liftIO $ addHomeModuleToFinder (hsc_FC ready)
    (hsc_home_unit ready) (GWIB (ms_mod_name summary) NotBoot) location
  imported <- parseImportDecl "import qualified FinalizedCoreFixture"
  setContext [IIDecl imported]
  answer <- dynCompileExpr
    "case FinalizedCoreFixture.evenBox 1 of FinalizedCoreFixture.Box value -> value"
  liftIO $ do
    assert ((fromDynamic answer :: Maybe Int) == Just 1)
      "direct bytecode lost its original recursive/private bindings"
    count <- readIORef frontends
    assert (count == 0) "direct bytecode invoked a module frontend"
    assert (case homeMod_object (hm_linkable executable) of Nothing -> True; _ -> False)
      "direct bytecode unexpectedly supplied native object code"
    original <- BS.readFile (exactPath artifact)
    assert (hexBytes (SHA256.hash original) == exactSha256 artifact)
      "direct bytecode changed its original admitted interface"
    present <- doesFileExist (work </> "FinalizedCoreFixture.hs")
    assert (not present) "direct bytecode restored original source"

-- This qualifies GHC's make handoff using a genuine finalized pair and the
-- fixture's structural certificate, independently of Rust certificate issuance.
makeViewChecks :: FilePath -> FilePath -> ExactIfaceArtifact -> BS.ByteString
  -> ModSummary -> CanonicalInterfaceProof -> IO ()
makeViewChecks libdir work artifact bytes summary proof = runGhc (Just libdir) $ do
  flags <- getSessionDynFlags
  _ <- setSessionDynFlags (ms_hspp_opts summary)
    { importPaths = [work], hiDir = Just work, objectDir = Just work
    , verbosity = verbosity flags }
  fresh <- getSession
  interfaces <- liftIO (requireRight =<< readExactIfaceArtifacts fresh [artifact])
  admitted <- liftIO (hydrateExactScope fresh interfaces)
  let typeDirectory = work </> "type-make-view"
      executableDirectory = work </> "executable-make-view"
      corePath = work </> "captured.core"
      executableSummary = summary {ms_hspp_opts =
        (ms_hspp_opts summary) {backend = interpreterBackend}}
  liftIO $ do
    createDirectory typeDirectory
    createDirectory executableDirectory
    removeFile corePath
  typeView <- liftIO (materializeCandidateCompilerView typeDirectory 0 admitted proof summary)
  liftIO $ do
    typeBytes <- BS.readFile (ml_hi_file (ms_location typeView))
    typeInterfaces <- requireRight =<< readExactIfaceArtifacts admitted
      [artifact {exactPath = ml_hi_file (ms_location typeView)
        , exactSha256 = hexBytes (SHA256.hash typeBytes)}]
    assert (case typeInterfaces of
        [(_,iface)] -> case mi_extra_decls iface of Nothing -> True; _ -> False
        _ -> False) "type-only make view loaded defining Core"
    BS.writeFile corePath (BS.take 1 bytes)
    refused <- try (materializeCandidateCompilerView executableDirectory 0
      admitted proof executableSummary) :: IO (Either CandidateCoreFailure ModSummary)
    assert (case refused of Left CandidateCoreBytesMismatch -> True; _ -> False)
      "make executable view accepted changed Core bytes"
    BS.writeFile corePath bytes
  (typeHome, _) <- loadView admitted typeView
  liftIO $ assert (case homeMod_bytecode (hm_linkable typeHome) of Nothing -> True; _ -> False)
    "type-only make handoff unnecessarily generated bytecode"
  executableView <- liftIO (materializeCandidateCompilerView executableDirectory 1
    admitted proof executableSummary)
  (executableHome, frontends) <- loadView admitted executableView
  liftIO $ assert (case homeMod_bytecode (hm_linkable executableHome) of
      Just linkable -> linkableModule linkable == ms_mod summary
      Nothing -> False) "source-free make handoff did not retain its executable input"
  liftIO $ assert (case homeMod_object (hm_linkable executableHome) of Nothing -> True; _ -> False)
    "source-free make handoff unexpectedly supplied native object code"
  loadedEnvironment <- getSession
  _ <- liftIO $ addHomeModuleToFinder (hsc_FC loadedEnvironment)
    (hsc_home_unit loadedEnvironment) (GWIB (ms_mod_name summary) NotBoot)
    (ms_location executableView)
  imported <- parseImportDecl "import qualified FinalizedCoreFixture"
  setContext [IIDecl imported]
  answer <- dynCompileExpr
    "case FinalizedCoreFixture.evenBox 1 of FinalizedCoreFixture.Box value -> value"
  liftIO $ do
    assert ((fromDynamic answer :: Maybe Int) == Just 1)
      "source-free make bytecode did not execute its original recursive/private bindings"
    assert (case homeMod_bytecode (hm_linkable executableHome) of
        Just linkable -> any (not . null . elemsFlatBag . bc_bcos)
          (foldMap linkablePartAllBCOs (linkableParts linkable))
        Nothing -> False) "source-free make demand did not generate real GHC bytecode"
    observed <- readIORef frontends
    assert (observed == 0) "source-free make handoff ran a module frontend"
    durable <- BS.readFile (exactPath artifact)
    assert (hexBytes (SHA256.hash durable) == exactSha256 artifact)
      "make cleanup changed or removed the durable original interface"
    present <- doesFileExist (work </> "FinalizedCoreFixture.hs")
    assert (not present) "make execution restored the original source"
  where
    loadView admitted view = do
      frontends <- liftIO (newIORef (0 :: Int))
      let hook :: TPhase result -> IO result
          hook phase@T_Hsc{} = do
            modifyIORef' frontends (+1)
            runPhase phase
          hook phase = runPhase phase
      setSession admitted {hsc_hooks = (hsc_hooks admitted)
        {runPhaseHook = Just (PhaseHook hook)}}
      loaded <- load' Nothing LoadAllTargets mkUnknownDiagnostic Nothing
        (mkModuleGraph [ModuleNode [] view])
      liftIO $ assert (case loaded of Succeeded -> True; Failed -> False)
        "source-free staged interface failed GHC make loading"
      env <- getSession
      home <- maybe (fail "source-free make result has no original HMI") pure
        (lookupHpt (hsc_HPT env) (ms_mod_name view))
      count <- liftIO (readIORef frontends)
      liftIO $ assert (count == 0) "staged make view invoked a module frontend"
      setSession env {hsc_hooks = hsc_hooks admitted}
      pure (home,frontends)

-- Local authority comes from the same completed GHC finalization capture.
-- This isolates Core attachment with a structural package/source fixture;
-- complete import receipt issuance belongs to the genuine pipeline tests.
captureLocalProof :: HscEnv -> FinalizedModule -> FilePath -> FilePath
  -> IO LocalFinalizedAdmission
captureLocalProof env finalized source work = do
  sourceProof <- sourceEvidence source
  let owner = mi_module (hm_iface (finalizedHomeModInfo finalized))
      name = moduleName owner
      key = (unitString (moduleUnit owner),moduleNameString name)
      originals = Map.singleton name finalized
      evidence = DependencyEvidence True True [sourceProof] [] []
        [DependencyModule (fst key) (snd key) False source [] ProductReady]
  interfaces <- newOriginalInterfaceArtifacts env originals [] work
  captured <- captureFinalizedModuleArtifacts interfaces env originals
    (Map.singleton name emptyPackageImports) evidence work
  maybe (fail "completed finalization did not issue its local admission") pure
    (Map.lookup key (finalizedLocalAdmissions captured))

-- The structural certificate exercises the matched decoder/validator with
-- compiler-produced interface/Core bytes; it does not prove Rust issuance.
captureProof :: HscEnv -> ExactIfaceArtifact -> FilePath -> BS.ByteString -> FilePath
  -> IO CanonicalInterfaceProof
captureProof env artifact source core work = do
  sourceBytes <- BS.readFile source
  let corePath = work </> "captured.core"
      packagePath = work </> "captured.packages"
      certificatePath = work </> "captured.certificate"
      manifestPath = work </> "captured.candidates"
      productPath = work </> "descriptor.tpmod"
      producer = replicate 64 'a'
      text = encodeString . T.pack
      list values = encodeListLen (fromIntegral (length values)) <> mconcat values
      sha = hexBytes . SHA256.hash
      homes = map unitIdString (Set.toAscList (hsc_all_home_unit_ids env))
      packages = encodePackageImports artifact emptyPackageImports
      certificate = toStrictByteString $ list
        [text "TPFINALMODULE",encodeWord 3,text "tidepool-ghc-finalized-module-v1",text producer
        ,list (map text homes),text (exactUnit artifact),text (exactModule artifact)
        ,text (sha sourceBytes),text (exactSha256 artifact),text (sha packages)
        ,text (sha core),list [],list [text "source-original",list []]]
      candidate = list
        (map text [exactUnit artifact,exactModule artifact,source,sha sourceBytes
          ,exactPath artifact,exactSha256 artifact,replicate 64 '0',sha BS.empty,replicate 64 '0']
        ++ [list [],list [],text packagePath,text (sha packages),text productPath,list []
          ,list (map text ["module",certificatePath,sha certificate,corePath,sha core])])
      manifest = toStrictByteString $ list
        [text "TPMCAN",text "10",list [],list [],list [candidate],list [list [],list []],text producer]
  BS.writeFile corePath core
  BS.writeFile packagePath packages
  BS.writeFile certificatePath certificate
  BS.writeFile productPath BS.empty
  BS.writeFile manifestPath manifest
  offered <- requireRight =<< readModuleCandidates manifestPath
  case offered of
    [candidate'] -> requireRight =<< validateCandidateCanonicalInterfaceProof producer
      [(artifact,packagePath,sha packages)] candidate'
    _ -> fail "candidate Core fixture has no exact offered owner"

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
