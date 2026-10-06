{-# LANGUAGE GADTs #-}

module FinalizedCoreTest (finalizedCoreChecks, postloadProviderFrontendOnce) where

import Control.Exception (bracket, try)
import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA256
import Data.ByteString qualified as BS
import Data.Dynamic (fromDynamic)
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC
import GHC.Core (Bind(..), bindersOfBinds)
import GHC.Core.InstEnv (is_dfun)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCon (tyConName)
import GHC.Cmm.CLabel (mkInitializerStubLabel)
import GHC.Data.FastString (fsLit)
import GHC.Types.ForeignStubs (ForeignStubs(..), CHeader(..), CStub(..))
import GHC.Utils.Outputable qualified as Outputable
import GHC.Driver.Main (loadIfaceByteCode)
import GHC.Driver.Make (load')
import GHC.Iface.Recomp
  ( MaybeValidated(..), CompileReason(..), RecompReason(..), checkOldIface )
import GHC.Driver.Pipeline.Execute (runPhase)
import GHC.Driver.Pipeline.Phases (TPhase(..), PhaseHook(..))
import GHC.Driver.Hooks (runPhaseHook)
import GHC.ByteCode.Types (CompiledByteCode(..))
import GHC.Data.FlatBag (elemsFlatBag)
import GHC.Linker.Types (linkableModule, linkableParts, linkablePartAllBCOs)
import GHC.Fingerprint.Type (Fingerprint(..))
import GHC.Types.Name (getOccString)
import GHC.Types.SptEntry (SptEntry(..))
import GHC.Types.TypeEnv (lookupTypeEnv)
import GHC.Types.Var (varName)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), HomeModLinkable(..), justBytecode, addToHpt, lookupHpt)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Unit.Module.ModIface
  ( ModIfaceBackend(..), mi_iface_hash
  , set_mi_usages, set_mi_final_exts, set_mi_module )
import GHC.Unit.Module.Deps (Usage(..))
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries')
import GHC.Unit.Types (unitString, toUnitId, stringToUnit, GenWithIsBoot(..))
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Driver.Env (HscEnv(..), hsc_HPT, hscUpdateHPT, hsc_dflags, hsc_home_unit)
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.ForeignSrcLang (ForeignSrcLang(..))
import Numeric (showHex)
import System.Directory
  ( copyFile, createDirectory, doesFileExist, getTemporaryDirectory
  , removeDirectoryRecursive, removeFile )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile, hPutStrLn, stderr)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), readExactIfaceArtifacts, hydrateExactScope )
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.FinalizedCore
import Tidepool.FinalizedModule (FinalizedModule(..))
import Tidepool.ExactScope
  ( CanonicalInterfaceProof, CanonicalInterfaceAdmission(..), admittedInterfaceCore
  , ExactScope(..), scopeInterfaces, readExactScope, scopeModuleInterfaceProofs
  , canonicalCertificateSha256 )
import Tidepool.FinalizedModuleArtifacts (finalizedLocalAdmissions)
import Tidepool.CompilerProducts (certifiedFinalizedArtifacts)
import Tidepool.GhcPipeline
  ( PreparedPipelineResult(..), PipelineResult(..), PipelineSelection(..)
  , CheckedEnvironmentResult(..), CompilePurpose(..), runPipelineSessionSelected
  , withResidentPipelineSelected )
import Tidepool.Test.GenuineCandidate
  ( FixtureCompilerInput(..), captureCompilerFixture, capturedCertifiedProducts
  , writeGenuineMetadataScope )
import Tidepool.HomeProducts
  ( CandidateCoreFailure(..), materializeCandidateCompilerView
  , admittedCompilerInterface, validateAdmittedInterfaceRequirements
  , validateCandidateInterfaceRequirements )
import Tidepool.PreparedStg (pmBindings, prepareModule)
import Tidepool.Session (SessionScope(..), emptySessionScope)
import SourceBootFixtureSupport
  ( withTiming, withScratch, captureDiagnostics, capturePreparedFixture
  , writeExecutionScope, hasIntResultLiteral )
import Tidepool.RetainedUnfoldings
  ( emptyRetainedContext, installRetainedUnfoldingsPlugin
  , scopeRetainedModuleGraph, scopeRetainedSummaryHscEnv )

-- | A new GHC session hydrates captured interface bytes after the only source
-- file has been removed. Its original Names, polymorphic types, recursive
-- groups, local class instance and CorePrep input must all survive.
finalizedCoreChecks :: IO ()
finalizedCoreChecks = bracket scratch removeDirectoryRecursive $ \work -> do
  let source = work </> "FinalizedCoreFixture.hs"
  copyFile "test-source-boot/fixtures/FinalizedCoreFixture.hs" source
  libdir <- getLibdir
  prepared <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing source [work] Nothing
  let env = prHscEnv (pprPipelineResult prepared)
      name = mkModuleName "FinalizedCoreFixture"
  finalized <- maybe (fail "compiler omitted its completed original finalization") pure
    (Map.lookup name (pprFinalizedModules prepared))
  summary <- case [item {ms_hspp_buf = Nothing}
      | ModuleNode _ item <- mgModSummaries' (hsc_mod_graph env), ms_mod_name item == name] of
    [item] -> pure item
    _ -> fail "compiler omitted its original module summary"
  capture <- captureCompilerFixture (FixtureCompilerInput work source [work]) prepared
  scopePath <- writeGenuineMetadataScope work ["FinalizedCoreFixture"] capture
  scope <- requireRight =<< readExactScope scopePath
  let guts = finalizedTidyGuts finalized
      owner = cg_module guts
      key = (unitString (moduleUnit owner), moduleNameString name)
      groups = bindingGroups guts
      tyconNames = map (getOccString . tyConName) (cg_tycons guts)
      instanceNames = map (getOccString . is_dfun) (finalizedCoreSiteInstances finalized)
      packages = cg_dep_pkgs guts
  proof <- maybe (fail "Rust admission omitted its canonical original proof") pure
    (Map.lookup key (scopeModuleInterfaceProofs scope))
  artifact <- case [item | (item,_,_) <- scopeInterfaces scope
      , (exactUnit item,exactModule item) == key] of
    [item] -> pure item
    _ -> fail "Rust admission omitted its exact original interface"
  localProof <- maybe (fail "completed capture omitted its local original admission") pure
    (Map.lookup key (finalizedLocalAdmissions
      (certifiedFinalizedArtifacts (capturedCertifiedProducts capture))))
  (corePath,coreSha) <- maybe (fail "Rust admission omitted its sealed original Core") pure
    (admittedInterfaceCore (ModuleInterfaceAdmission proof))
  bytes <- BS.readFile corePath
  assert (hexBytes (SHA256.hash bytes) == coreSha) "issued Core seal changed before consumption"
  rawBytes <- requireRight =<< captureFinalizedCore env finalized work
  assert (bytes == rawBytes) "certification did not retain the original finalized Core bytes"
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
  -- A second request receives the same immutable capture without compiling it.
  -- The make view and direct hydration then own independent scope deliveries.
  makeScopePath <- writeGenuineMetadataScope work ["FinalizedCoreFixture"] capture
  assert (makeScopePath /= scopePath) "repeated consumption reused a mutable request scope"
  makeScope <- requireRight =<< readExactScope makeScopePath
  makeProof <- maybe (fail "second Rust admission omitted its original proof") pure
    (Map.lookup key (scopeModuleInterfaceProofs makeScope))
  assert (canonicalCertificateSha256 makeProof == canonicalCertificateSha256 proof)
    "independent delivery changed the original canonical identity"
  removeFile source
  exists <- doesFileExist source
  assert (not exists) "source-free roundtrip kept its source"
  runGhc (Just libdir) $ do
    _ <- setSessionDynFlags (ms_hspp_opts summary)
      { backend = noBackend, ghcLink = NoLink, importPaths = [work]
      , hiDir = Just work, objectDir = Just work }
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
      forM_ [ModuleInterfaceAdmission proof, LocalInterfaceAdmission localProof] $ \admission -> do
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
      prepared <- prepareModule env (ms_location summary) mempty finalized
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
  makeViewChecks libdir work artifact bytes summary makeProof
  putStrLn "finalized Core: executed one case; source-free STG, direct bytecode and GHC make execution with zero module frontends; staged type-only/Core views, durable interface, native requirements, Core seal and unsupported metadata checked"
  where
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
-- Rust-issued canonical proof from the same immutable compiler capture.
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
  (corePath, coreSha) <- maybe (fail "canonical proof omitted its captured Core") pure
    (admittedInterfaceCore (ModuleInterfaceAdmission proof))
  let typeDirectory = work </> "type-make-view"
      executableDirectory = work </> "executable-make-view"
      executableSummary = summary {ms_hspp_opts =
        (ms_hspp_opts summary) {backend = interpreterBackend}}
  liftIO $ do
    issuedCore <- BS.readFile corePath
    assert (issuedCore == bytes && hexBytes (SHA256.hash issuedCore) == coreSha)
      "canonical proof did not retain its original compiler Core"
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
  -- A genuine pipeline original seals its module-scoped retained policy even
  -- when the selected retained set is empty. An ordinary cold GHC session is a
  -- different compiler profile; it must not silently recompile that original.
  missingPolicy <- liftIO (checkOldIface
    (scopeRetainedSummaryHscEnv typeView admitted) typeView Nothing)
  liftIO $ case missingPolicy of
    OutOfDateItem (RecompBecause PluginsChanged) _ -> pure ()
    OutOfDateItem reason _ -> fail ("cold make refused before its missing retained policy: "
      ++ Outputable.renderWithContext Outputable.defaultSDocContext (Outputable.ppr reason))
    UpToDateItem _ -> fail "cold make accepted a missing original retained policy"
  let makeEnvironment = installRetainedUnfoldingsPlugin emptyRetainedContext admitted
  (typeHome, _) <- loadView makeEnvironment typeView
  liftIO $ assert (case homeMod_bytecode (hm_linkable typeHome) of Nothing -> True; _ -> False)
    "type-only make handoff unnecessarily generated bytecode"
  executableView <- liftIO (materializeCandidateCompilerView executableDirectory 1
    makeEnvironment proof executableSummary)
  (executableHome, frontends) <- loadView makeEnvironment executableView
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
      decision <- liftIO (checkOldIface
        (scopeRetainedSummaryHscEnv view admitted) view Nothing)
      liftIO $ case decision of
        UpToDateItem _ -> pure ()
        OutOfDateItem reason _ -> fail ("source-free make profile refused its original: "
          ++ Outputable.renderWithContext Outputable.defaultSDocContext (Outputable.ppr reason))
      frontends <- liftIO (newIORef (0 :: Int))
      let hook :: TPhase result -> IO result
          hook phase@T_Hsc{} = do
            modifyIORef' frontends (+1)
            runPhase phase
          hook phase = runPhase phase
      setSession admitted {hsc_hooks = (hsc_hooks admitted)
        {runPhaseHook = Just (PhaseHook hook)}}
      loaded <- load' Nothing LoadAllTargets mkUnknownDiagnostic Nothing
        (scopeRetainedModuleGraph (mkModuleGraph [ModuleNode [] view]))
      liftIO $ assert (case loaded of Succeeded -> True; Failed -> False)
        "source-free staged interface failed GHC make loading"
      env <- getSession
      home <- maybe (fail "source-free make result has no original HMI") pure
        (lookupHpt (hsc_HPT env) (ms_mod_name view))
      count <- liftIO (readIORef frontends)
      liftIO $ assert (count == 0) "staged make view invoked a module frontend"
      setSession env {hsc_hooks = hsc_hooks admitted}
      pure (home,frontends)

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

-- The exact plan restores its lexical environment before compiling fresh
-- providers. Their effectful quoter must run once in that canonical frontend,
-- and the checked/native consumer must use the resulting finalized product.
postloadProviderFrontendOnce :: IO ()
postloadProviderFrontendOnce = withTiming $ withScratch $ \producerRoot -> withScratch $ \work -> do
  let fixture name = "test-source-boot/fixtures" </> name
      retained = "MetadataQuoteSupport"
      provider = work </> "PostloadQuotedProvider.hs"
      target = work </> "PostloadProviderConsumer.hs"
      counter = work </> "postload-quoter-executions"
      owners = ["PostloadObservedQuoter", "PostloadQuotedProvider"]
      captured phase name diagnostics = length
        [line | line <- lines diagnostics, line == "tidepool-canonical-" ++ phase
          ++ " module=" ++ name]
      requireOnce diagnostics = do
        actual <- lines <$> readFile counter
        unless (actual == ["42"] && all (\name -> captured "frontend" name diagnostics == 1
            && captured "finalization" name diagnostics == 1) owners) $ do
          hPutStrLn stderr ("post-load quoter executions=" ++ show actual ++ "\n" ++ diagnostics)
          fail "post-load fresh provider replayed its quoter or lost canonical capture"
        assert (Text.pack "tidepool-count name=exact_execution_fresh_provider_compiles count=2 "
            `Text.isInfixOf` Text.pack diagnostics)
          "regression did not enter the two-provider post-load pass"
        assert (captured "frontend" retained diagnostics == 0
            && captured "finalization" retained diagnostics == 0)
          "post-load execution replayed its retained original"
      executable env name = case lookupHpt (hsc_HPT env) (mkModuleName name) of
        Nothing -> False
        Just home -> case homeMod_bytecode (hm_linkable home) of
          Just _ -> True
          Nothing -> False
  -- Issue an actual canonical original in a separate workspace. The consumer
  -- receives its immutable interface/Core custody, not its source or a memo.
  let originalSource = producerRoot </> "MetadataQuoteSupport.hs"
  copyFile (fixture "MetadataQuoteSupport.hs") originalSource
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing originalSource [producerRoot] Nothing
  originalFixture <- capturePreparedFixture producerRoot original
  scopePath <- writeExecutionScope work originalFixture [retained]
  forM_ ["PostloadObservedQuoter.hs", "PostloadProviderConsumer.hs"] $ \name ->
    copyFile (fixture name) (work </> name)
  quotedSource <- Text.pack <$> readFile (fixture "PostloadQuotedProvider.hs")
  writeFile provider (Text.unpack (Text.replace "{{QUOTER_COUNTER}}" (Text.pack counter) quotedSource))
  let scope = emptySessionScope {ssRoot=work, ssExactScope=Just scopePath}
  writeFile counter ""
  withResidentPipelineSelected [work] $ \compile -> do
    (checked, diagnostics) <- captureDiagnostics $
      compile CheckedEnvironment Set.empty GeneralCompile (Just scope) target [work] Nothing
    requireOnce diagnostics
    assert (all (executable (crHscEnv checked)) (retained : owners))
      "checked consumer lost its retained or fresh providers' real GHC bytecode"
    assert (case crResultType checked of Just _ -> True; Nothing -> False)
      "checked provider consumer lost its result type"
  -- A separate fresh compiler makes this native check independent of the
  -- preceding checked request's resident memo and executable products.
  writeFile counter ""
  withResidentPipelineSelected [work] $ \compile -> do
    (prepared, diagnostics) <- captureDiagnostics $
      compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope) target [work] Nothing
    requireOnce diagnostics
    assert (all (\name -> Map.member (mkModuleName name) (pprFinalizedModules prepared)) owners)
      "native consumer omitted a fresh provider's canonical finalized product"
    quoted <- maybe (fail "native consumer omitted its quoted provider") pure
      (Map.lookup (mkModuleName "PostloadQuotedProvider") (pprFinalizedModules prepared))
    assert (hasIntResultLiteral 42 (cg_binds (finalizedTidyGuts quoted)))
      "native finalized provider lost the actual quoted integer42"
    assert (all (executable (prHscEnv (pprPipelineResult prepared))) (retained : owners))
      "native consumer lost its retained or fresh providers' real GHC bytecode"
  putStrLn "post-load providers: genuine retained root, actual quoter42 once in each checked/native cycle, canonical capture and executable bytecode passed"
