{-# LANGUAGE GADTs #-}

module FinalizedCoreTest (finalizedCoreChecks, postloadProviderFrontendOnce, memoIngressSelectionHistory, snapshotExecutableHook, unownedUpToDateDependency, mixedCandidateLoads, canonicalLoadGraphHistories, agentPageDisplayFinalizationHistory) where

import Control.Exception (SomeException, IOException, bracket, try, evaluate, throwIO)
import Control.Concurrent (forkFinally, killThread, newEmptyMVar, readMVar, putMVar, takeMVar)
import Control.Monad (forM, forM_, unless, when)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA256
import Data.ByteString qualified as BS
import Data.Dynamic (fromDynamic)
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC
import Test.QuickCheck qualified as QC
import GHC.Core (Bind(..), bindersOfBinds)
import GHC.Core.InstEnv (is_dfun)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCon (tyConName)
import GHC.Cmm.CLabel (mkInitializerStubLabel)
import GHC.Data.FastString (fsLit)
import GHC.Types.ForeignStubs (ForeignStubs(..), CHeader(..), CStub(..))
import GHC.Utils.Outputable qualified as Outputable
import GHC.Driver.Session (targetWays_)
import GHC.Platform.Ways (Way(WayProf), hasWay, addWay, removeWay)
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
import GHC.Types.SourceFile (HscSource(..))
import GHC.Types.SptEntry (SptEntry(..))
import GHC.Types.TypeEnv (lookupTypeEnv, mkTypeEnv, typeEnvElts)
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
import Data.List (isInfixOf, isPrefixOf, stripPrefix, intercalate)
import System.Directory
  ( copyFile, createDirectory, createDirectoryIfMissing, doesFileExist, getTemporaryDirectory
  , removeDirectoryRecursive, removeFile, listDirectory )
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.FilePath ((</>), takeDirectory)
import System.IO (hClose, openTempFile, hPutStrLn, stderr)
import Tidepool.ArtifactBytes (ArtifactBytes, artifactBytes, captureArtifactBytes)
import System.Mem.StableName (makeStableName)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), readExactIfaceArtifacts, hydrateExactScope, serializeOriginalInterface
  , newOriginalInterfaceArtifacts, originalInterfaceBody )
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.FinalizedCore
import Tidepool.FinalizedModule (FinalizedModule, newFinalizedModule, finalizedHomeModInfo, finalizedTidyGuts, captureFinalizedInterface)
import Tidepool.ExactScope
  ( CanonicalInterfaceProof, CanonicalInterfaceAdmission(..), admittedInterfaceCore
  , ExactScope, scopeManifestPath, scopeInterfaces, readExactScope, scopeModuleInterfaceProofs
  , canonicalCertificatePath, canonicalCertificateSha256, readScopedInterfaces, revalidateExactScope, captureCanonicalProofs, CanonicalInterfaceUse(..)
  , validateCanonicalInterfaceProof, admittedInterfaceCoreBytes, admittedInterfaceCoreBody, extendExactScopeInputs, ExactInterfaceEvidence(..) )
import Tidepool.FinalizedModuleArtifacts (captureFinalizedModuleArtifacts, materializeFinalizedModuleArtifactsWithOutputs, finalizedLocalAdmissions, materializeFinalizedModuleArtifacts, localFinalizedInterface, localFinalizedCore, localFinalizedCoreBody, revalidateLocalFinalizedAdmission, FinalizedModuleArtifacts)
import Tidepool.CompilerProducts (certifiedFinalizedArtifacts, newPreparedOriginalInterfaceArtifacts)
import Tidepool.GhcPipeline
  ( pprPipelineResult, pprFinalizedModules, pprPackageImports, preparedFreshDependencies, prHscEnv, prResultType, PipelineSelection(..)
  , pprAcceptedCandidates, revalidatePreparedCandidateInputs, CheckedEnvironmentResult(..), CompilePurpose(..), runPipelineSessionSelected, withResidentPipelineSelected, withSourceImportIntents )
import Tidepool.Test.GenuineCandidate
  ( FixtureCompilerInput(..), captureCompilerFixture, capturedCertifiedProducts
  , writeGenuineMetadataScope, writeGenuineEmptyMetadataScope, writeGenuineCandidateManifestFor )
import Tidepool.HomeProducts
  ( CandidateCoreFailure(..), materializeCandidateCompilerView
  , admittedCompilerInterface, validateCapturedAdmittedCore, validateAdmittedInterfaceRequirements
  , validateCandidateInterfaceRequirements )
import Tidepool.PreparedStg (pmBindings, prepareModule)
import Tidepool.ModuleCandidates (candidateModule)
import Tidepool.Session (SessionScope(..), emptySessionScope)
import Tidepool.Binders (analyzeCellWithFlags, CellSourcePlan(..))
import SourceBootCases (admitCheckedScope)
import SourceBootFixtureSupport
  ( withTiming, withScratch, captureDiagnostics, capturePreparedFixture
  , writeExecutionScope, hasIntResultLiteral, manifest )
import Tidepool.RetainedUnfoldings
  ( emptyRetainedContext, installRetainedUnfoldingsPlugin
  , scopeRetainedModuleGraph, scopeRetainedSummaryHscEnv )

-- Equal authored source and dependency seals must not acquire a different
-- PageDisplay dictionary when unrelated home modules change compilation order.
-- Compare the issued bytes directly, independently of admission or recovery.
agentPageDisplayFinalizationHistory :: IO ()
agentPageDisplayFinalizationHistory = withScratch $ \work -> do
  library <- lookupEnv "TIDEPOOL_PRELUDE_DIR" >>= maybe (fail "missing native standard library") pure
  effects <- lookupEnv "TIDEPOOL_TEST_EFFECTS_DIR" >>= maybe (fail "missing native effects source") pure
  let roots = [work, library, takeDirectory library </> "actors", effects]
      owner = mkModuleName "Tidepool.Actors.Internal.Agent"
      imports = ["Tidepool.Actors.Internal.Agent", "Tidepool.Actors.Internal.ExomonadDriver",
        "Tidepool.Actors.Exomonad", "Tidepool.Command"]
      body prepared = do
        finalized <- maybe (fail "compiler omitted Agent finalization") pure
          (Map.lookup owner (pprFinalizedModules prepared))
        let env = prHscEnv (pprPipelineResult prepared)
        interface <- serializeOriginalInterface env work (hm_iface (finalizedHomeModInfo finalized))
        core <- requireRight =<< captureFinalizedCore finalized
        pure (interface, artifactBytes core)
      target index imported = do
        let name = "AgentPageHistory" ++ show index
            source = work </> name ++ ".hs"
        writeFile source (unlines ["module " ++ name ++ " where",
          "import Tidepool.Actors.Internal.Agent ()", "import " ++ imported ++ " ()",
          "__result :: Int", "__result = 0"])
        pure source
  bodies <- forM (zip [0 :: Int ..] imports) $ \(index, imported) -> do
    source <- target index imported
    prepared <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing source roots Nothing
    fresh <- body prepared
    if index /= 0 then pure [fresh] else do
      fixture <- capturePreparedFixture work prepared
      let support = map moduleNameString
            (Map.keys (Map.delete owner
              (Map.delete (mkModuleName "AgentPageHistory0") (pprFinalizedModules prepared))))
      scopePath <- writeGenuineMetadataScope work support fixture
      scope <- requireRight =<< readExactScope scopePath
      admitted <- admitCheckedScope scope roots []
      let session = emptySessionScope {ssRoot=work,ssExactScope=Just (scopeManifestPath admitted)}
      laterSource <- target (10 :: Int) "Tidepool.Actors.Exomonad"
      later <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty
        (ExactScopeCompile GeneralCompile admitted) (Just session) laterSource roots Nothing
      hydrated <- body later
      pure [fresh, hydrated]
  let histories = concat bodies
      fingerprint (interface, core) = (hexBytes (SHA256.hash interface), hexBytes (SHA256.hash core))
  forM_ (zip [1 :: Int ..] (drop 1 histories)) $ \(index, captured) ->
    assert (captured == head histories)
      ("Agent canonical interface/Core changed with unrelated history " ++ show index)
  putStrLn ("Agent PageDisplay: four fresh histories and a hydrated support history retain identical canonical bytes "
    ++ show (map fingerprint histories))

-- Exercise actual codec work through one genuine completed pair. Each fresh
-- owner has independent deferred representations even when its body is equal.
deferredFinalizationChecks :: FilePath -> HscEnv -> FinalizedModule -> IO ()
deferredFinalizationChecks work env original = do
  let home = finalizedHomeModInfo original
      guts = finalizedTidyGuts original
      fresh = newFinalizedModule env home guts
  owner <- fresh
  ((first, second), coreCounts) <- withTiming $ captureDiagnostics $
    concurrentCaptures (captureFinalizedCore owner >>= requireRight)
  sameBody "concurrent Core consumers rebuilt held bytes" first second
  assert (captureCount "finalized_core_encoding_attempts" coreCounts == 1
      && captureCount "finalized_core_encoding_completions" coreCounts == 1
      && captureCount "finalized_core_capture_reuses" coreCounts == 1)
    "same-owner concurrent demands did not share one Core encoding"
  ((left, right), interfaceCounts) <- withTiming $ captureDiagnostics $
    concurrentCaptures (captureFinalizedInterface owner)
  sameBody "concurrent interface consumers rebuilt held bytes" left right
  assert (captureCount "finalized_interface_encoding_attempts" interfaceCounts == 1
      && captureCount "finalized_interface_encoding_completions" interfaceCounts == 1)
    "same-owner concurrent demands did not share one interface encoding"
  distinct <- fresh
  (distinctBody, distinctCounts) <- withTiming $ captureDiagnostics $
    captureFinalizedCore distinct >>= requireRight
  assert (captureCount "finalized_core_encoding_attempts" distinctCounts == 1
      && artifactBytes distinctBody == artifactBytes first)
    "a distinct fresh semantic owner borrowed another owner's completed encoding"
  let flags = hsc_dflags env
      originalWays = targetWays_ flags
      changedWays = (if hasWay originalWays WayProf then removeWay else addWay) WayProf originalWays
      receiving = env {hsc_dflags = flags {targetWays_ = changedWays}}
      name = moduleName (mi_module (hm_iface home))
  expected <- serializeOriginalInterface env work (hm_iface home)
  changed <- serializeOriginalInterface receiving work (hm_iface home)
  assert (expected /= changed) "profile control did not change the interface codec input"
  profileOwner <- fresh
  receivingView <- newOriginalInterfaceArtifacts receiving (Map.singleton name profileOwner) [] work
  issued <- originalInterfaceBody receivingView (mi_module (hm_iface home))
    >>= maybe (fail "receiving view lost the finalized original") pure
  assert (artifactBytes issued == expected)
    "receiving request profile changed the original owner's interface encoding"
  retryOwner <- fresh
  ((refused, retryBody), retryCounts) <- withTiming $ captureDiagnostics $ do
    refused <- bracket (lookupEnv "TMPDIR") restoreTemporary $ \_ -> do
      setEnv "TMPDIR" (work </> "absent-interface-scratch")
      try (captureFinalizedInterface retryOwner) :: IO (Either IOException ArtifactBytes)
    retryBody <- captureFinalizedInterface retryOwner
    pure (refused, retryBody)
  assert (case refused of Left _ -> True; Right _ -> False)
    "interface scratch IO failure control did not refuse"
  assert (artifactBytes retryBody == expected
      && captureCount "finalized_interface_encoding_completions" retryCounts == 1)
    "failed first interface capture poisoned the same semantic owner"
 where
  restoreTemporary = maybe (unsetEnv "TMPDIR") (setEnv "TMPDIR")

concurrentCaptures :: IO a -> IO (a, a)
concurrentCaptures action = do
  start <- newEmptyMVar
  first <- newEmptyMVar
  second <- newEmptyMVar
  let acquire = mapM (\done -> forkFinally (readMVar start >> action) (putMVar done)) [first, second]
      result done = takeMVar done >>= either throwIO pure
  bracket acquire (mapM_ killThread) $ \_ -> do
    putMVar start ()
    (,) <$> result first <*> result second

sameBody :: String -> ArtifactBytes -> ArtifactBytes -> IO ()
sameBody label first second = do
  left <- evaluate (artifactBytes first) >>= makeStableName
  right <- evaluate (artifactBytes second) >>= makeStableName
  assert (left == right) label

captureCount :: String -> String -> Integer
captureCount name diagnostics = sum
  [read value | line <- lines diagnostics
    , ("tidepool-count name=" ++ name ++ " ") `isPrefixOf` line
    , field <- words line, Just value <- [stripPrefix "count=" field]]

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
  deferredFinalizationChecks work env finalized
  let heldDirectory = work </> "capture-before-materialization"
  createDirectory heldDirectory
  heldInterfaces <- newPreparedOriginalInterfaceArtifacts prepared heldDirectory
  ((_, issuedOutputs), publicationCounts) <- withTiming $ captureDiagnostics $ do
    held <- captureFinalizedModuleArtifacts heldInterfaces env (pprFinalizedModules prepared)
      (pprPackageImports prepared) (preparedFreshDependencies prepared) heldDirectory
    capturedFiles <- listDirectory heldDirectory
    assert (null capturedFiles) "held capture wrote finalized packet destination files"
    materializeFinalizedModuleArtifactsWithOutputs heldDirectory held
  packetFiles <- listDirectory heldDirectory
  assert (not (null issuedOutputs) && length packetFiles == length issuedOutputs
      && captureCount "finalized_payload_materializations" publicationCounts == fromIntegral (length packetFiles))
    "first publication materialized a finalized packet payload more than once"
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
  originalCoreBody <- admittedInterfaceCoreBody (ModuleInterfaceAdmission proof)
  rawBytes <- requireRight =<< captureFinalizedCore finalized
  assert (bytes == artifactBytes rawBytes) "certification did not retain the original finalized Core bytes"
  let emptyStubs = ForeignStubs (CHeader Outputable.empty) (CStub Outputable.empty [] [])
      initializer = mkInitializerStubLabel (cg_module guts) (fsLit "canonical_test_init")
      nonemptyStubs =
        [ForeignStubs (CHeader (Outputable.text "extern void canonical_test(void);")) (CStub Outputable.empty [] [])
        ,ForeignStubs (CHeader Outputable.empty) (CStub (Outputable.text "void canonical_test(void) {}") [] [])
        ,ForeignStubs (CHeader Outputable.empty) (CStub Outputable.empty [initializer] [])
        ,ForeignStubs (CHeader Outputable.empty) (CStub Outputable.empty [] [initializer])]
  emptyOwner <- newFinalizedModule env (finalizedHomeModInfo finalized) guts {cg_foreign = emptyStubs}
  emptyCapture <- requireRight =<< captureFinalizedCore emptyOwner
  assert (artifactBytes emptyCapture == bytes) "semantically empty GHC foreign stubs changed canonical Core"
  forM_ nonemptyStubs $ \stubs -> do
    refusedOwner <- newFinalizedModule env (finalizedHomeModInfo finalized) guts {cg_foreign = stubs}
    refusal <- captureFinalizedCore refusedOwner
    assert (case refusal of Left (FinalizedCoreUnsupported ForeignExportStubs) -> True; _ -> False)
      "nonempty foreign header/body/initializer/finalizer was discarded"
  forM_ (take 1 (bindersOfBinds (cg_binds guts))) $ \binder -> do
    refusedOwner <- newFinalizedModule env (finalizedHomeModInfo finalized)
      guts {cg_spt_entries = [SptEntry binder (Fingerprint 1 2)]}
    refusal <- captureFinalizedCore refusedOwner
    assert (case refusal of Left (FinalizedCoreUnsupported StaticPointerEntries) -> True; _ -> False)
      "static-pointer metadata was discarded instead of explicitly refused"
  refusedOwner <- newFinalizedModule env (finalizedHomeModInfo finalized)
    guts {cg_foreign_files = [(LangC, work </> "unavailable.c")]}
  refusal <- captureFinalizedCore refusedOwner
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
  promotionScopeChecks env work scope proof
  capturedConsumerChecks libdir work scope artifact bytes summary proof
  -- Relocated receipt views may disappear without revoking the compiler-issued
  -- byte owner. A new materialization writes held bodies; its fresh destination
  -- still refuses a conflicting file instead of silently accepting that file.
  let capturedArtifacts = certifiedFinalizedArtifacts (capturedCertifiedProducts capture)
      firstDirectory = work </> "held-finalized-first"
      nextDirectory = work </> "held-finalized-next"
  firstArtifacts <- materializeFinalizedModuleArtifacts firstDirectory capturedArtifacts
  firstAdmission <- maybe (fail "materialized local admission absent") pure
    (Map.lookup key (finalizedLocalAdmissions firstArtifacts))
  let (firstIface,firstPackages,_) = localFinalizedInterface firstAdmission
      firstPaths = exactPath firstIface : firstPackages : maybe [] (pure . fst) (localFinalizedCore firstAdmission)
  forM_ firstPaths $ \path -> BS.writeFile path "changed receiving artifact view"
  revalidateLocalFinalizedAdmission firstAdmission >>= requireRight
  forM_ firstPaths removeFile
  nextArtifacts <- materializeFinalizedModuleArtifacts nextDirectory firstArtifacts
  nextAdmission <- maybe (fail "rematerialized local admission absent") pure
    (Map.lookup key (finalizedLocalAdmissions nextArtifacts))
  (nextCore,_) <- maybe (fail "rematerialization lost Core reference") pure (localFinalizedCore nextAdmission)
  BS.readFile nextCore >>= \actual -> assert (actual == bytes) "rematerialization reopened deleted old Core"
  heldCore <- maybe (fail "local admission did not hold Core") pure (localFinalizedCoreBody firstAdmission)
  assert (artifactBytes heldCore == bytes) "materialization changed held Core identity"
  BS.writeFile nextCore "conflicting fresh output"
  collision <- try (materializeFinalizedModuleArtifacts nextDirectory firstArtifacts)
    :: IO (Either SomeException FinalizedModuleArtifacts)
  assert (case collision of Left _ -> True; Right _ -> False) "fresh materialization accepted conflicting output bytes"
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
      local <- admittedCompilerInterface env (LocalInterfaceAdmission localProof) (ms_mod summary)
      assert (mi_module local == ms_mod summary && case mi_extra_decls local of
          Just _ -> True; Nothing -> False)
        "compiler-issued local finalization lost its owned defining Core"
      attached <- admittedCompilerInterface env (ModuleInterfaceAdmission proof) (ms_mod summary)
      assert (mi_module attached == ms_mod summary && case mi_extra_decls attached of
          Just _ -> True; Nothing -> False)
        "captured executable interface lost its original owner or defining Core"
      wrongOwner <- try (admittedCompilerInterface env (ModuleInterfaceAdmission proof)
        (mkModule (stringToUnit "another-home-unit") (ms_mod_name summary)))
        :: IO (Either CandidateCoreFailure ModIface)
      assert (case wrongOwner of Left CandidateCoreHomeMissing -> True; _ -> False)
        "captured executable interface accepted a different exact unit"
      (finalized, recoveryCounts) <- withTiming $ captureDiagnostics $ do
        recovered <- requireRight =<< decodeFinalizedCore env home (ms_location summary) originalCoreBody
        held <- requireRight =<< captureFinalizedCore recovered
        sameBody "cold recovery recaptured its original Core" originalCoreBody held
        pure recovered
      assert (captureCount "finalized_core_admitted_seeds" recoveryCounts == 1
          && captureCount "finalized_core_encoding_attempts" recoveryCounts == 0)
        "cold recovery encoded Core instead of retaining its admitted body"
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
      missingBinder <- case [binder | binder <- bindersOfBinds (cg_binds guts)
          , getOccString binder == "identity", isExternalName (varName binder)] of
        [binder] -> pure binder
        _ -> fail "missing-binding control lost its genuine external identity binder"
      let missingName = varName missingBinder
          details = hm_details home
          incompleteTypes = mkTypeEnv
            [thing | thing <- typeEnvElts (md_types details), case thing of
              AnId identifier -> varName identifier /= missingName
              _ -> True]
          incompleteHome = home {hm_details = details {md_types = incompleteTypes}}
      (incomplete, incompleteCounts) <- withTiming $ captureDiagnostics $
        decodeFinalizedCore env incompleteHome (ms_location summary) originalCoreBody
      assert (case incomplete of
          Left (FinalizedCoreInterfaceBindingMissing occurrence) -> occurrence == getOccString missingBinder
          _ -> False)
        "canonical companion admitted Core whose external binder was absent from its original interface"
      assert (captureCount "finalized_core_admitted_seeds" incompleteCounts == 0
          && captureCount "finalized_core_encoding_attempts" incompleteCounts == 0)
        "refused native pair seeded or re-encoded its original Core"
      prepared <- prepareModule env (ms_location summary) mempty finalized
      assert (not (null (pmBindings prepared))) "cold canonical Core produced no STG"
      let owner = cg_module guts
          wrongHome = home { hm_iface = set_mi_module
            (mkModule (moduleUnit owner) (mkModuleName "OtherOriginal")) (hm_iface home) }
      wrongOwner <- decodeFinalizedCore env wrongHome (ms_location summary) originalCoreBody
      assert (case wrongOwner of Left FinalizedCoreOwnerMismatch -> True; _ -> False)
        "canonical companion accepted a different exact owner"
      let backend = mi_final_exts (hm_iface home)
          fingerprint = mi_iface_hash backend
          different = if fingerprint == Fingerprint 0 0 then Fingerprint 1 0 else Fingerprint 0 0
          wrongVersion = home { hm_iface = set_mi_final_exts
            backend {mi_iface_hash = different} (hm_iface home) }
      mismatch <- decodeFinalizedCore env wrongVersion (ms_location summary) originalCoreBody
      assert (case mismatch of Left FinalizedCoreInterfaceMismatch -> True; _ -> False)
        "canonical companion accepted a different native interface version"
      malformed <- decodeFinalizedCore env home (ms_location summary) (captureArtifactBytes (BS.take 1 bytes))
      assert (case malformed of Left FinalizedCoreDecodeFailure{} -> True; _ -> False)
        "truncated canonical companion did not fail closed"
  directBytecodeChecks libdir work artifact summary (ModuleInterfaceAdmission proof)
  makeViewChecks libdir work makeScope artifact bytes summary makeProof
  putStrLn "finalized Core: executed one case; source-free STG, direct bytecode and GHC make execution with zero module frontends; staged type-only/Core views, durable interface, native requirements, Core seal and unsupported metadata checked"
  where
    scratch = do
      root <- getTemporaryDirectory
      (path, handle) <- openTempFile root "tidepool-finalized-core"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- The real interface and executable consumers must use admitted bytes even
-- when old artifact paths change or disappear. The retained body stays authoritative. The
-- second decode in one NameCache reuses the iface; another NameCache cannot.
capturedConsumerChecks :: FilePath -> FilePath -> ExactScope -> ExactIfaceArtifact
  -> BS.ByteString -> ModSummary -> CanonicalInterfaceProof -> IO ()
capturedConsumerChecks libdir work scope artifact coreBytes summary proof = do
  original <- BS.readFile (exactPath artifact)
  (corePath,_) <- maybe (fail "snapshot consumer omitted defining Core") pure
    (admittedInterfaceCore (ModuleInterfaceAdmission proof))
  (_,diagnostics) <- withTiming $ captureDiagnostics $
    bracket (BS.writeFile (exactPath artifact) (BS.singleton 0) >> BS.writeFile corePath (BS.singleton 0))
      (\_ -> BS.writeFile (exactPath artifact) original >> BS.writeFile corePath coreBytes) $ \_ -> do
        let consume = runGhc (Just libdir) $ do
              _ <- setSessionDynFlags (ms_hspp_opts summary)
                {backend=noBackend,ghcLink=NoLink,importPaths=[work],hiDir=Just work,objectDir=Just work}
              fresh <- getSession
              loaded <- liftIO (requireRight =<< readScopedInterfaces fresh scope [artifact])
              _ <- liftIO (requireRight =<< readScopedInterfaces fresh scope [artifact])
              admitted <- liftIO (hydrateExactScope fresh loaded)
              liftIO $ do
                attached <- admittedCompilerInterface admitted (ModuleInterfaceAdmission proof) (ms_mod summary)
                assert (mi_module attached == ms_mod summary && case mi_extra_decls attached of
                    Just _ -> True; Nothing -> False) "snapshot executable consumer reopened changed Core"
                terminal <- revalidateExactScope admitted scope
                requireRight terminal
        consume
        consume
  let count marker = length [() | line <- lines diagnostics, marker `isInfixOf` line]
  assert (count "exact_iface_read_calls.FinalizedCoreFixture" == 2
      && count "exact_iface_decode_reuse.FinalizedCoreFixture" == 2)
    ("request decoder crossed a NameCache boundary or repeated an admitted interface decode: "
      ++ show (count "exact_iface_read_calls.FinalizedCoreFixture",count "exact_iface_decode_reuse.FinalizedCoreFixture")
      ++ "\n" ++ diagnostics)

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
makeViewChecks :: FilePath -> FilePath -> ExactScope -> ExactIfaceArtifact -> BS.ByteString
  -> ModSummary -> CanonicalInterfaceProof -> IO ()
makeViewChecks libdir work scope artifact bytes summary proof = runGhc (Just libdir) $ do
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
    capturedView <- materializeCandidateCompilerView executableDirectory 0 admitted proof executableSummary
    assert (ml_hi_file (ms_location capturedView) /= exactPath artifact)
      "captured executable make view retained its producer path"
    validateCapturedAdmittedCore (ModuleInterfaceAdmission proof)
    requireRight =<< revalidateExactScope admitted scope
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
  seededProvider <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile
    Nothing provider [work,producerRoot] (Just work)
  let seededEnv = prHscEnv (pprPipelineResult seededProvider)
  retainRegressionInterfaces "postload-seed" seededEnv
  quoterSummary <- case [summary | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph seededEnv)
      , ms_mod_name summary == mkModuleName "PostloadObservedQuoter"] of
    [summary] -> pure summary
    _ -> fail "post-load seed lacks its ordinary quoter summary"
  currentQuoter <- checkOldIface (scopeRetainedSummaryHscEnv quoterSummary seededEnv) quoterSummary Nothing
  assert (case currentQuoter of UpToDateItem _ -> True; _ -> False)
    "post-load regression did not seed an up-to-date provider interface"
  writeFile counter ""
  withResidentPipelineSelected [work] $ \compile -> do
    (checked, diagnostics) <- captureDiagnostics $
      compile CheckedEnvironment Set.empty GeneralCompile (Just scope) target [work] (Just work)
    retainRegressionInterfaces "postload-checked" (crHscEnv checked)
    requireOnce diagnostics
    assert (all (executable (crHscEnv checked)) (retained : owners))
      "checked consumer lost its retained or fresh providers' real GHC bytecode"
    assert (case crResultType checked of Just _ -> True; Nothing -> False)
      "checked provider consumer lost its result type"
    writeFile counter ""
    (warm, warmDiagnostics) <- captureDiagnostics $
      compile CheckedEnvironment Set.empty GeneralCompile (Just scope) target [work] (Just work)
    actual <- lines <$> readFile counter
    assert (actual == ["42"] && captured "frontend" "PostloadObservedQuoter" warmDiagnostics == 0
        && captured "finalization" "PostloadObservedQuoter" warmDiagnostics == 0
        && captured "frontend" "PostloadQuotedProvider" warmDiagnostics == 1
        && captured "finalization" "PostloadQuotedProvider" warmDiagnostics == 1)
      ("post-load memo provider replayed or untracked quoter was reused: " ++ warmDiagnostics)
    assert (all (executable (crHscEnv warm)) (retained : owners))
      "warm direct provider ingress lost selected memo bytecode"
  -- A separate fresh compiler makes this native check independent of the
  -- preceding checked request's resident memo and executable products.
  writeFile counter ""
  withResidentPipelineSelected [work] $ \compile -> do
    (prepared, diagnostics) <- captureDiagnostics $
      compile (PreparedProducts Nothing) Set.empty GeneralCompile (Just scope) target [work] (Just work)
    retainRegressionInterfaces "postload-native" (prHscEnv (pprPipelineResult prepared))
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

-- A byte-exact return to A must select its completed version, even when B was
-- the most recent source. Diagnostic status is independent of frontend work.
memoIngressSelectionHistory :: IO ()
memoIngressSelectionHistory = withTiming $ bracket (lookupEnv "TIDEPOOL_MEMO_TRACE")
  (maybe (unsetEnv "TIDEPOOL_MEMO_TRACE") (setEnv "TIDEPOOL_MEMO_TRACE")) $ \_ -> do
  setEnv "TIDEPOOL_MEMO_TRACE" "1"
  withScratch $ \work -> do
    let fixture name = "test-source-boot/fixtures" </> name
        helper = work </> "MetadataQuoteSupport.hs"
        target = work </> "MetadataCurrentSourceTarget.hs"
        selectionRow diagnostics = case
            [line | line <- lines diagnostics
            , Text.pack "tidepool-memo-cycle-graph " `Text.isPrefixOf` Text.pack line
            , "module=MetadataQuoteSupport" `elem` words line
            , "source_kind=OrdinaryHomeSource" `elem` words line] of
          [line] -> pure (words line)
          _ -> fail ("memo selection omitted its exact owner row: " ++ diagnostics)
        observe compile expected expectedFrontends = do
          (prepared, diagnostics) <- captureDiagnostics $
            compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing target [work] Nothing
          row <- selectionRow diagnostics
          assert (("selection=" ++ expected) `elem` row)
            ("memo ingress selected another state: " ++ diagnostics)
          whenSelected expected $ assert ("executable_observation=observed" `elem` row)
            "selected source version omitted its actual executable-capacity decision"
          assert (length [line | line <- lines diagnostics
              , line == "tidepool-canonical-frontend module=MetadataQuoteSupport"] == expectedFrontends)
            "memo ingress diagnostic disagreed with actual canonical frontend work"
          assert (isJustResult prepared) "memo history omitted its target result"
          case [value | field <- row
              , Just value <- [Text.stripPrefix (Text.pack "selected_originating_cycle=") (Text.pack field)]] of
            [value] -> pure value
            _ -> fail "memo selection omitted its originating cycle"
        whenSelected expected action = if expected == "MemoSelected" then action else pure ()
        isJustResult prepared = case prResultType (pprPipelineResult prepared) of
          Just _ -> True
          Nothing -> False
    copyFile (fixture "MetadataCurrentSourceTarget.hs") target
    original <- BS.readFile (fixture "MetadataQuoteSupport.hs")
    BS.writeFile helper original
    withResidentPipelineSelected [work] $ \compile -> do
      _ <- observe compile "MemoOwnerAbsent" 1
      firstA <- observe compile "MemoSelected" 0
      copyFile (fixture "MetadataQuoteSupportChanged.hs") helper
      _ <- observe compile "MemoIngressAbsent" 1
      BS.writeFile helper original
      returnedA <- observe compile "MemoSelected" 0
      assert (firstA /= Text.pack "none" && returnedA == firstA)
        "return to byte-exact A selected another originating cycle"
  putStrLn "memo ingress A,A,B,A: absent/selected/changed-ingress/original-selected and actual frontend1/0/1/0 passed"

-- Drive the actual cpAfterLoad executable hook with missing and replaced Core
-- paths. The fresh quoter observes retained bytecode, then optionally restores
-- the producer before the real terminal scope check.
snapshotExecutableHook :: IO ()
snapshotExecutableHook = withTiming $ withScratch $ \producer -> withScratch $ \work -> do
  let fixtures = "test-source-boot/fixtures"
      source = producer </> "MetadataQuoteSupport.hs"
      target = work </> "CapturedCoreConsumer.hs"
      saved = work </> "saved-original.core"
      marker = work </> "executed-original"
  copyFile (fixtures </> "MetadataQuoteSupport.hs") source
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing source [producer] Nothing
  fixture <- capturePreparedFixture producer original
  path <- writeExecutionScope work fixture ["MetadataQuoteSupport"]
  scope <- requireRight =<< readExactScope path
  proof <- maybe (fail "executable hook fixture lacks its original") pure
    (Map.lookup ("main","MetadataQuoteSupport") (scopeModuleInterfaceProofs scope))
  (corePath,_) <- maybe (fail "executable hook fixture lacks Core") pure
    (admittedInterfaceCore (ModuleInterfaceAdmission proof))
  bytes <- BS.readFile corePath
  copyFile corePath saved
  copyFile (fixtures </> "CapturedCoreQuoter.hs") (work </> "CapturedCoreQuoter.hs")
  template <- Text.pack <$> readFile (fixtures </> "CapturedCoreConsumer.hs")
  let session = emptySessionScope {ssRoot=work,ssExactScope=Just path}
      settings restore = show (marker, if restore then Just (saved,corePath) else Nothing)
      consume replace restore = bracket
        (replace corePath >> writeFile marker "" >> writeFile target
          (Text.unpack (Text.replace "{{SNAPSHOT_SETTINGS}}" (Text.pack (settings restore)) template)))
        (\_ -> BS.writeFile corePath bytes) $ \_ -> do
          checked <- try (runPipelineSessionSelected CheckedEnvironment Set.empty
            (ExactScopeCompile GeneralCompile scope) (Just session) target [work] Nothing)
            :: IO (Either IOException CheckedEnvironmentResult)
          observed <- lines <$> readFile marker
          assert (observed == ["42"])
            "actual executable hook failed to supply original bytecode before quoter restoration"
          case checked of
            Right result -> requireRight =<< revalidateExactScope (crHscEnv result) scope
            Left failure -> fail ("owned executable capture failed after old path mutation: " ++ show failure)
  forM_ [removeFile, \path' -> BS.writeFile path' (BS.singleton 0)] $ \replace -> do
    consume replace True
    consume replace False
  putStrLn "snapshot executable hook: deleted/replaced Core consumed through actual post-load bytecode and terminal publication passed without restoration"


-- Metadata proof validation, executable promotion and scope extension form one
-- custody history. Exact byte budgets cover every captured interface, certificate, sidecar and Core.
promotionScopeChecks :: HscEnv -> FilePath -> ExactScope -> CanonicalInterfaceProof -> IO ()
promotionScopeChecks env work scope proof = do
  let key = ("main","FinalizedCoreFixture")
  row@(iface,packages,_) <- case scopeInterfaces scope of
    [row] -> pure row
    _ -> fail "promotion fixture requires one original interface"
  core@(corePath,_) <- maybe (fail "promotion fixture lacks defining Core") pure
    (admittedInterfaceCore (ModuleInterfaceAdmission proof))
  bytes <- BS.readFile corePath
  let config = "TIDEPOOL_REQUEST_CAPTURE_BYTES"
      restoreConfig = maybe (unsetEnv config) (setEnv config)
      metadataProof = requireRight =<< validateCanonicalInterfaceProof scope key
        (canonicalCertificatePath proof) (canonicalCertificateSha256 proof) (Just core)
  bracket (lookupEnv config) restoreConfig $ \_ -> do
    descriptor <- bracket (removeFile corePath) (\_ -> BS.writeFile corePath bytes) $ \_ -> do
      descriptor <- metadataProof
      metadata <- captureCanonicalProofs Nothing [(descriptor,MetadataInterfaceUse)]
      assert (length metadata == 1) "metadata-only promotion required absent Core"
      unowned <- try (admittedInterfaceCoreBytes (ModuleInterfaceAdmission descriptor))
        :: IO (Either IOException BS.ByteString)
      assert (either (const True) (const False) unowned)
        "raw metadata descriptor silently acquired executable Core"
      unavailable <- try (captureCanonicalProofs Nothing [(descriptor,ExecutableInterfaceUse)])
        :: IO (Either IOException [CanonicalInterfaceProof])
      assert (either (const True) (const False) unavailable)
        "executable promotion accepted missing defining Core"
      pure descriptor
    originalFiles <- mapM BS.readFile [exactPath iface,packages,canonicalCertificatePath proof]
    let promotionBudget = sum (map BS.length originalFiles) + BS.length bytes
    setEnv config (show (promotionBudget - 1))
    exceeded <- try (captureCanonicalProofs Nothing [(descriptor,ExecutableInterfaceUse)])
      :: IO (Either IOException [CanonicalInterfaceProof])
    assert (either (const True) (const False) exceeded)
      "canonical proof inputs bypassed request capture budget"
    setEnv config (show promotionBudget)
    promoted <- captureCanonicalProofs Nothing [(descriptor,ExecutableInterfaceUse),(descriptor,ExecutableInterfaceUse)]
    emptyPath <- writeGenuineEmptyMetadataScope work
    metadataBytes <- mapM BS.readFile
      [emptyPath,exactPath iface,packages,canonicalCertificatePath proof]
    let fullBudget = sum (map BS.length metadataBytes) + BS.length bytes
        offered = [(row,ModuleInterfaceEvidence captured) | captured <- promoted]
        extend limit = do
          setEnv config (show limit)
          base <- requireRight =<< readExactScope emptyPath
          extendExactScopeInputs base offered
    let paths = [exactPath iface,packages,canonicalCertificatePath proof,corePath]
        originals = originalFiles ++ [bytes]
    extended <- bracket (mapM_ removeFile paths) (\_ -> sequence_ (zipWith BS.writeFile paths originals)) $ \_ -> do
      extended <- requireRight =<< extend fullBudget
      selected <- maybe (fail "scope extension lost its captured proof") pure
        (Map.lookup key (scopeModuleInterfaceProofs extended))
      consumed <- admittedInterfaceCoreBytes (ModuleInterfaceAdmission selected)
      assert (consumed == bytes)
        "scope extension reread deleted promoted Core instead of transferring its owner"
      interfaces <- requireRight =<< readScopedInterfaces env extended [iface]
      assert (map fst interfaces == [iface]) "promoted interface hydration reopened deleted producer metadata"
      terminal <- revalidateExactScope env extended
      requireRight terminal
      smaller <- extend (fullBudget - 1)
      assert (either (const True) (const False) smaller)
        "scope extension ignored receiving aggregate budget"
      pure extended
    requireRight =<< revalidateExactScope env extended
  putStrLn "canonical proof history: absent metadata Core, explicit promotion, shared exact budget and transfer across deleted producer paths passed"

-- Persistent interfaces can be current for GHC while the resident universe
-- has never admitted their finalized owners. A fresh importer must observe the
-- interface produced by exactly one canonical dependency finalization.
unownedUpToDateDependency :: IO ()
unownedUpToDateDependency = withTiming $ withScratch $ \work -> do
  let fixtures = "test-source-boot/fixtures"
      owner = work </> "MetadataQuoteSupport.hs"
      target = work </> "MetadataCurrentSourceTarget.hs"
      name = mkModuleName "MetadataQuoteSupport"
      count phase diagnostics = length [line | line <- lines diagnostics,
        line == "tidepool-canonical-" ++ phase ++ " module=MetadataQuoteSupport"]
  copyFile (fixtures </> "MetadataQuoteSupport.hs") owner
  copyFile (fixtures </> "MetadataCurrentSourceTarget.hs") target
  seeded <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile
    Nothing owner [work] (Just work)
  let env = prHscEnv (pprPipelineResult seeded)
  retainRegressionInterfaces "unowned-seed" env
  summary <- case [item | ModuleNode _ item <- mgModSummaries' (hsc_mod_graph env),
      ms_mod_name item == name] of
    [item] -> pure item
    _ -> fail "persistent dependency seed omitted its summary"
  decision <- checkOldIface (scopeRetainedSummaryHscEnv summary env) summary Nothing
  case decision of
    UpToDateItem _ -> pure ()
    OutOfDateItem reason _ -> fail ("regression dependency is not GHC-up-to-date: "
      ++ Outputable.showSDocUnsafe (Outputable.ppr reason))
  withResidentPipelineSelected [work] $ \compile -> do
    (prepared, diagnostics) <- captureDiagnostics $
      compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing target [work] (Just work)
    retainRegressionInterfaces "unowned-native" (prHscEnv (pprPipelineResult prepared))
    unless (count "frontend" diagnostics == 1 && count "finalization" diagnostics == 1) $ do
      hPutStrLn stderr diagnostics
      fail "unowned GHC-up-to-date dependency skipped canonical load finalization"
    capture <- capturePreparedFixture work prepared
    scopePath <- writeGenuineMetadataScope work ["MetadataQuoteSupport", "MetadataCurrentSourceTarget"] capture
    scope <- requireRight =<< readExactScope scopePath
    requireRight =<< revalidateExactScope (prHscEnv (pprPipelineResult prepared)) scope
    (_, warmDiagnostics) <- captureDiagnostics $
      compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing target [work] (Just work)
    assert (count "frontend" warmDiagnostics == 0 && count "finalization" warmDiagnostics == 0)
      "admitted completed dependency replayed its source frontend"
    -- This actual current-source import runs checkOldIface over the complete
    -- captured dependency graph, under the same resident summary history.
    admitted <- admitCheckedScope scope [work] []
    let consumer = work </> "CurrentOriginalConsumer.hs"
        session = emptySessionScope {ssRoot=work,ssExactScope=Just (scopeManifestPath admitted)}
    writeFile consumer "module CurrentOriginalConsumer where\nimport MetadataCurrentSourceTarget qualified as Original\n__result :: Int\n__result = Original.__result\n"
    parsed <- analyzeCellWithFlags (hsc_dflags (prHscEnv (pprPipelineResult prepared))) ""
      "import MetadataCurrentSourceTarget qualified as Original\nOriginal.__result"
      >>= either (fail . show) pure
    let purpose = withSourceImportIntents (cellPlanPrologue parsed) GeneralCompile
    _ <- compile CheckedEnvironment Set.empty (ExactScopeCompile purpose admitted)
      (Just session) consumer [work] Nothing
    copyFile (fixtures </> "MetadataQuoteSupportChanged.hs") owner
    (_, changedDiagnostics) <- captureDiagnostics $
      compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing target [work] (Just work)
    assert (count "frontend" changedDiagnostics == 1 && count "finalization" changedDiagnostics == 1)
      "changed dependency did not earn a new canonical finalization"
    copyFile (fixtures </> "MetadataQuoteSupport.hs") owner
    (_, restoredDiagnostics) <- captureDiagnostics $
      compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing target [work] (Just work)
    assert (count "frontend" restoredDiagnostics == 0 && count "finalization" restoredDiagnostics == 0)
      "byte-exact restoration did not retain its original completed memo"
  putStrLn "unowned up-to-date dependency: GHC disk hit, canonical frontend/finalization once, immediate exact graph validation and memo hit passed"

-- Retain both the issued in-memory interface and the physical ingress bytes.
-- This runs while the producer's scratch and captured file descriptors live.
retainRegressionInterfaces :: String -> HscEnv -> IO ()
retainRegressionInterfaces label env = do
  destination <- lookupEnv "TIDEPOOL_TEST_ARTIFACT_ROOT"
  forM_ destination $ \root -> do
    let directory = root </> "canonical-load" </> label
    createDirectoryIfMissing True directory
    forM_ [summary | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph env)
      , ms_hsc_src summary == HsSrcFile] $ \summary ->
      forM_ (lookupHpt (hsc_HPT env) (ms_mod_name summary)) $ \home -> do
        let iface = hm_iface home
            prefix = directory </> moduleNameString (ms_mod_name summary)
        when (mi_module iface == ms_mod summary) $ do
          bytes <- serializeOriginalInterface env directory iface
          BS.writeFile (prefix ++ ".issued.hi") bytes
          present <- doesFileExist (ml_hi_file (ms_location summary))
          when present (copyFile (ml_hi_file (ms_location summary)) (prefix ++ ".disk.hi"))
          writeFile (prefix ++ ".usages") (unlines (map renderUsage (mi_usages iface)))
          writeFile (prefix ++ ".sha256") (hexBytes (SHA256.hash bytes) ++ "\n")

renderUsage :: Usage -> String
renderUsage usage = case usage of
  UsageHomeModule{} -> show ("home", render (usg_unit_id usage), render (usg_mod_name usage),
    usg_mod_hash usage, [(render occurrence, fingerprint) | (occurrence,fingerprint) <- usg_entities usage],
    usg_exports usage, usg_safe usage)
  UsageHomeModuleInterface{} -> show ("home-interface", render (usg_unit_id usage),
    render (usg_mod_name usage), usg_iface_hash usage)
  UsagePackageModule{} -> show ("package", render (usg_mod usage), usg_mod_hash usage, usg_safe usage)
  UsageFile{} -> show ("file", render (usg_file_path usage), usg_file_hash usage, usg_file_label usage)
  UsageMergedRequirement{} -> show ("merged", render (usg_mod usage), usg_mod_hash usage)
  where
    render :: Outputable.Outputable a => a -> String
    render = Outputable.showSDocUnsafe . Outputable.ppr

-- GHC Make consumes both candidate kinds in one graph when a splice requires
-- executable capacity. The interface-only branch must keep its staged owner.
mixedCandidateLoads :: IO ()
mixedCandidateLoads = withTiming $ withScratch $ \work -> do
  let target = work </> "MixedCandidateTarget.hs"
      seed = work </> "MixedCandidateSeed.hs"
      selected = ["MetadataOwner","MetadataQuoteSupport"]
      source name = "module " ++ name ++ " where\nimport MetadataTarget qualified as Metadata\nimport MetadataQuotedTarget qualified as Quoted\n__result :: Int\n__result = Metadata.__result + Quoted.__result\n"
  forM_ ["MetadataOwner.hs","MetadataTarget.hs","MetadataQuoteSupport.hs","MetadataQuoter.hs","MetadataQuotedTarget.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  -- Candidate publication requires a source-reusable seed. Execute the
  -- quotation only in the fresh consumer, after the two native owners issue.
  writeFile seed "module MixedCandidateSeed where\nimport MetadataTarget qualified as Metadata\nimport MetadataQuoter qualified as Quoter\n__result :: Int\n__result = Metadata.__result\nquoter = Quoter.answer\n"
  writeFile target (source "MixedCandidateTarget")
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing seed [work] Nothing
  fixture <- capturePreparedFixture work original
  writeGenuineCandidateManifestFor selected work fixture
  (prepared, diagnostics) <- captureDiagnostics $
    runPipelineSessionSelected (PreparedProducts (Just (manifest work))) Set.empty GeneralCompile
      Nothing target [work] Nothing
  retainRegressionInterfaces "mixed-candidate" (prHscEnv (pprPipelineResult prepared))
  assert (Set.fromList (map candidateModule (pprAcceptedCandidates prepared)) == Set.fromList selected)
    ("mixed graph did not admit both certified candidate kinds: " ++ diagnostics)
  assert ("tidepool-count name=candidate_executable_required count=1 " `isInfixOf` diagnostics)
    ("mixed graph did not require exactly its helper executable: " ++ diagnostics)
  forM_ ["candidate_compiler_view_attempt", "candidate_compiler_view_complete"] $ \counter ->
    assert (length [() | line <- lines diagnostics
        , ("tidepool-count name=" ++ counter ++ " count=1 ") `isInfixOf` line] == length selected
      && any (("tidepool-count name=" ++ counter ++ " count=0 ") `isInfixOf`) (lines diagnostics))
      ("mixed graph did not stage every actual candidate make view: " ++ diagnostics)
  forM_ selected $ \name -> assert (not (any (\phase ->
      ("tidepool-canonical-" ++ phase ++ " module=" ++ name) `elem` lines diagnostics)
      ["frontend","finalization"])) "mixed graph replayed a certified candidate frontend"
  let env = prHscEnv (pprPipelineResult prepared)
      hasCode name = case lookupHpt (hsc_HPT env) (mkModuleName name) of
        Nothing -> False
        Just home -> let linkable = hm_linkable home
          in case (homeMod_bytecode linkable, homeMod_object linkable) of
            (Nothing,Nothing) -> False
            _ -> True
  assert (hasCode "MetadataQuoteSupport" && not (hasCode "MetadataOwner"))
    "mixed graph changed the independent metadata/executable capacities"
  captured <- capturePreparedFixture work prepared
  forM_ (Map.elems (finalizedLocalAdmissions
      (certifiedFinalizedArtifacts (capturedCertifiedProducts captured)))) $ \admission ->
    revalidateLocalFinalizedAdmission admission >>= requireRight
  revalidatePreparedCandidateInputs prepared
  putStrLn "mixed candidates: staged metadata plus canonical-Core bytecode, no candidate frontend, captured originals and candidate validation passed"

-- Small authored graphs run through the real compiler. The oracle rescans
-- their import relation to identify the source changes that must invalidate
-- importers; cold, repeated, changed and restored histories observe actual
-- canonical frontend/finalization events and captured interface graph seals.
canonicalLoadGraphHistories :: IO ()
canonicalLoadGraphHistories = withTiming $ do
  let graph = do
        size <- QC.chooseInt (2,4)
        edges <- mapM (\i -> QC.sublistOf [0..i-1]) [0..size-1]
        changed <- QC.chooseInt (0,size-1)
        pure (edges,changed)
      shrink (edges,changed) = [(take i edges ++ [reduced] ++ drop (i+1) edges,changed)
        | (i,imports) <- zip [0..] edges, reduced <- QC.shrinkList (const []) imports]
  result <- QC.quickCheckWithResult QC.stdArgs {QC.maxSuccess=6, QC.maxShrinks=12}
    (QC.forAllShrink graph shrink $ \scenario@(edges,changed) ->
      QC.classify (any null (drop 1 edges)) "independent dependency" $
      QC.classify (any ((>1) . length) edges) "shared dependency graph" $
      QC.counterexample (show scenario) (QC.ioProperty (runHistory edges changed)))
  unless (QC.isSuccess result) (fail "canonical load graph/history diverged from its import oracle")
  where
    runHistory edges changed = withScratch $ \work -> do
      let nodes = [0..length edges-1]
          name i = "CanonicalGraph" ++ show i
          path i = work </> name i ++ ".hs"
          moduleSource i value = unlines
            (["module " ++ name i ++ " (value) where"]
            ++ ["import " ++ name child ++ " qualified as N" ++ show child | child <- edges !! i]
            ++ ["value :: Int","value = " ++ intercalate " + "
              (show value : ["N" ++ show child ++ ".value" | child <- edges !! i])])
          targetSource targetName = unlines
            (["module " ++ targetName ++ " where"]
            ++ ["import " ++ name i ++ " qualified as N" ++ show i | i <- nodes]
            ++ ["__result :: Int","__result = " ++ intercalate " + " ["N" ++ show i ++ ".value" | i <- nodes]])
          affected = grow (Set.singleton changed)
            where
              grow seen =
                let next = seen `Set.union` Set.fromList
                      [i | (i,children) <- zip nodes edges, any (`Set.member` seen) children]
                in if next == seen then seen else grow next
          captureEvents phase diagnostics = [i | i <- nodes,
            ("tidepool-canonical-" ++ phase ++ " module=" ++ name i) `elem` lines diagnostics]
          once phase diagnostics i = length [() | line <- lines diagnostics,
            line == "tidepool-canonical-" ++ phase ++ " module=" ++ name i] == 1
          observe compile label expected = do
            (prepared,diagnostics) <- captureDiagnostics $
              compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
                (work </> "CanonicalGraphConsumer.hs") [work] (Just work)
            forM_ ["frontend","finalization"] $ \phase ->
              assert (Set.fromList (captureEvents phase diagnostics) == expected
                && all (once phase diagnostics) (Set.toList expected))
                ("graph " ++ label ++ " canonical work disagreed with the import oracle: " ++ diagnostics)
            fixture <- capturePreparedFixture work prepared
            scopePath <- writeGenuineMetadataScope work ("CanonicalGraphConsumer" : map name nodes) fixture
            scope <- requireRight =<< readExactScope scopePath
            requireRight =<< revalidateExactScope (prHscEnv (pprPipelineResult prepared)) scope
      forM_ nodes $ \i -> writeFile (path i) (moduleSource i (i+1))
      writeFile (work </> "CanonicalGraphSeed.hs") (targetSource "CanonicalGraphSeed")
      writeFile (work </> "CanonicalGraphConsumer.hs") (targetSource "CanonicalGraphConsumer")
      _ <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile Nothing
        (work </> "CanonicalGraphSeed.hs") [work] (Just work)
      withResidentPipelineSelected [work] $ \compile -> do
        observe compile "unowned disk" (Set.fromList nodes)
        observe compile "memo hit" Set.empty
        writeFile (path changed) (moduleSource changed 100)
        observe compile "source change" affected
        writeFile (path changed) (moduleSource changed (changed+1))
        observe compile "source restoration" Set.empty
      pure True
