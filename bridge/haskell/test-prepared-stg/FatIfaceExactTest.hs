module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)
import Test.Tasty qualified as Tasty

import Control.Concurrent (ThreadId, forkFinally, killThread, yield)
import Control.Concurrent.MVar
  ( MVar, newEmptyMVar, putMVar, readMVar, takeMVar )
import Control.Exception (SomeException, ErrorCall, try, bracket, onException, throwIO, evaluate)
import Control.Monad (forM, forM_, foldM, unless)
import Control.Monad.IO.Class (liftIO)
import Data.Bits (shiftL, testBit)
import Data.IORef (newIORef, readIORef, atomicModifyIORef')
import qualified Data.IntMap.Strict as IntMap
import Data.List (sortOn)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC
import GHC.Core (CoreBind, CoreExpr, Bind(..), maybeUnfoldingTemplate)
import GHC.Builtin.Types (intTy, intTyCon)
import GHC.Core.TyCon (tyConName)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Driver.Env (HscEnv, hsc_HPT, hscUpdateHPT)
import GHC.Driver.Session (gopt_unset, updOptLevel)
import GHC.Types.Id (Id, realIdUnfolding, isFCallId)
import GHC.IfaceToCore (tcTopIfaceBindings, typecheckIface)
import GHC.Tc.Utils.Monad (initIfaceCheck, initIfaceLcl)
import GHC.Types.Avail (availNames)
import GHC.Types.TypeEnv (emptyTypeEnv, typeEnvIds)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Types (unitString)
import GHC.Utils.Outputable (text)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Types.Name (mkExternalName, mkSystemName, nameModule_maybe, nameModule, nameOccName, isExternalName)
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString)
import GHC.Types.Unique (mkUnique)
import GHC.Types.Var (varName, isLocalId, isId)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Unit.Module.ModIface (ModIface, mi_extra_decls, mi_exports, set_mi_extra_decls)
import System.Directory
  ( createDirectoryIfMissing
  , getTemporaryDirectory
  , removeFile
  , removePathForcibly
  )
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Timeout (timeout)
import System.Mem.StableName (makeStableName)
import Tidepool.FatIface
  ( FatIfaceLookup(..)
  , FatIfaceMissing(..)
  , FatIfaceComponentLookup(..)
  , fatOriginalOwner, fatComponentBindings, fatComponentAllBinders, fatComponentOrdinals
  , fatSelectionVersion, fatSelectionComponents
  , fatSelectionDemandedGroupCount, fatSelectionPreparedGroupCount
  , lookupFatIfaceExact, lookupFatIfaceBodies
  , lookupFatIfaceComponents, privateOriginalDependencies
  , readHomeInterface, readPackageInterface, newFatIfaceCache
  , ownerInterfaceEntries, newOwnerInterfaceCache, copyOwnerInterfaceCache
  , lookupOwnerInterface, sameOwnerInterfaceContext, mergeOwnerInterfaceCaches, selectOwnerInterfaceCaches, evictOwnerInterfaceMatching
  )
import Tidepool.FatIface.Internal
  ( newLoadCache, lookupLoadCache, lookupCompletedLoadCache
  , copyLoadCache, mergeLoadCaches, selectLoadCaches, evictLoadCache
  , privateComponents
  )
import GHC.Conc (ThreadStatus(..), BlockReason(..), threadStatus)
import Tidepool.ExactHydration (forkExactContext, freshExactContext, ExactIfaceArtifact(..), hydrateExactScope,
  RequestIfaceDecoder, CapturedInterfaceInputs, verifiedInterfaceProducts, newRequestIfaceDecoder, readCapturedExactIfaceClosureWithCheckedValues,
  selectVerifiedExactInterfaceProducts, verifiedExactInterface, capturedInterfaceInputs,
  hydrateInterfaceInputs, retainInstalledInterfaceInputs, hydratedExactInterfaces, hydratedExactEnvironment, hydratedExactInputs, restoreHydratedInterfaceEnvironment,
  emptyHydratedInterfaceClosure, hydratedClosureAsCapturedInputs, selectHydratedInterfaceInputs, admitCheckedValueInterface,
  readVerifiedExactIfaceClosure, captureCompletedInterfaceInputs, mergeCapturedInterfaceInputs, capturedSessionInterfaceFor, checkCapturedSessionInterface,
  newOriginalInterfaceArtifactsWithSessionCaptures, serializeOriginalInterface,
  InterfaceInputFailure(..))
import Tidepool.Session
  ( SessionModule(..), SessionModuleKind(..), Generation(..), sessionModuleString, sessionHiPath
  , mkThinSessionIface, writeSessionIface, injectSessionIfaceWithBindings
  , capturedSessionInterfaceBody, capturedSessionCompilerProduct, sameCapturedSessionInterfaceOwner )
import Tidepool.ArtifactBytes (ArtifactBytes, captureArtifactBytes, artifactSha256)
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.PackageWitness (PackageImportEvidence(..), packageImportRoot, encodePackageImports)
import qualified Data.ByteString as BS
import GHC.Unit.Home.ModInfo (HomePackageTable, lookupHpt, hm_iface, filterHpt)
import Tidepool.Resolve (ExactBodyLookup(..), recoverExactBody)
import Tidepool.PreparedStg
  (newPreparedBodyCache, prepareRecoveredBodies, pmBindings, pmStableTopSpellings
  , newPreparedComponentUnitsPreparer, runPreparedComponentTask, preparedComponentModules, selectPreparedBodyCaches)
import Tidepool.ExecutionProjection (topBinders)
import Tidepool.GhcPipeline
  ( withResidentCompilerScopes, scopedCompile, PipelineSelection(..), CompilePurpose(..)
  , pprFinalizedModules, pprPipelineResult, prHscEnv )
import Tidepool.FinalizedModule
  ( finalizedHomeModInfo, captureFinalizedInterface, homeInterfaceUsageOwners )
import Tidepool.FinalizedModule.Internal (retainCompletedModuleInterface)

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "test-prepared-stg"
  [ Tasty.withResource acquireProducedFixtures releaseProducedFixtures $ \produced ->
      testGroup "exact interface cache lifecycle"
        [ Tasty.withResource (produced >>= checkPrivateFixture) (const (pure ())) $ \private ->
            testGroup "private fat Core"
              [ testCase "captured HOME survives unlink" (private >>= verifyCapturedHome)
              , testCase "exact closure and genuine dangling GlobalId" (private >>= verifyPrivateClosure)
              , testCase "independent component selection oracle" (private >>= verifyComponentSelection)
              , testCase "prepared component reuse and context isolation" (private >>= verifyPreparedReuse)
              ]
        , Tasty.withResource (produced >>= checkThinFixture) (const (pure ())) $ \thin ->
            testGroup "thin TYPE"
              [ testCase "captured aliases and NameCache isolation" (thin >>= verifyCapturedThin)
              , testCase "genuine INLINE cannot supply defining Core" (thin >>= verifyThinInlineRefusal)
              ]
        , Tasty.withResource (produced >>= checkInlineFixture) (const (pure ())) $ \inline ->
            testCase "retained HOME and absent-owner INLINE refusal" (inline >>= verifyRetainedHome)
        ]
  , testCase "hydrated original allocation and exact finalization ownership" verifyHydratedOriginalOwners
  , testCase "completed interface capture ownership" verifyCompletedCaptures
  , testCase "concurrent interface cache lifecycle" verifyCacheConcurrency
  , testCase "selected completed cache merge" verifyCacheMerges
  , testCase "canonical private component graphs" verifyPrivateComponents
  ]

-- Exhaust all directed graphs through four vertices and compare weak
-- connectivity with a separate edge-walking oracle. Reversing edge and row
-- construction order must preserve the canonical roster.
verifyPrivateComponents :: IO ()
verifyPrivateComponents = forM_ [0 .. 4] $ \size -> do
  let vertices = [0 .. size - 1]
      possibleEdges = [(source, target) | source <- vertices, target <- vertices]
      graphCount = (1 :: Int) `shiftL` length possibleEdges
  forM_ [0 .. graphCount - 1] $ \mask -> do
    let edges = [edge | (index, edge) <- zip [0..] possibleEdges, testBit mask index]
        rows = [(source, [target | (from, target) <- edges, from == source]) | source <- vertices]
        graph = Map.fromList [(source, Set.fromList targets) | (source, targets) <- rows]
        reversedGraph = Map.fromList
          [(source, Set.fromList (reverse targets)) | (source, targets) <- reverse rows]
        permute vertex = size - 1 - vertex
        permutedEdges = [(permute source, permute target) | (source, target) <- edges]
        permutedRows =
          [(permute source, [permute target | target <- targets]) | (source, targets) <- rows]
        permutedGraph = Map.fromList
          [(source, Set.fromList targets) | (source, targets) <- permutedRows]
        actual = privateComponents graph
        expected = oracleComponents vertices edges
        relabeled = sortOn Set.findMin (map (Set.map permute) actual)
    assert (actual == expected) ("weak component oracle mismatch: " ++ show (size, mask, actual, expected))
    assert (actual == privateComponents reversedGraph)
      ("component roster depended on graph construction order: " ++ show (size, mask))
    assert (privateComponents permutedGraph == relabeled)
      ("component roster did not follow a vertex permutation: " ++ show (size, mask, permutedEdges))
  where
    oracleComponents vertices edges = visit (Set.fromList vertices) []
      where
        visit remaining components = case Set.minView remaining of
          Nothing -> reverse components
          Just (seed, _) ->
            let component = reach Set.empty [seed]
            in visit (remaining `Set.difference` component) (component : components)
        reach seen [] = seen
        reach seen (vertex : pending)
          | vertex `Set.member` seen = reach seen pending
          | otherwise =
              let adjacent = [right | (left, right) <- edges, left == vertex]
                    ++ [left | (left, right) <- edges, right == vertex]
              in reach (Set.insert vertex seen) (adjacent ++ pending)

-- The independent-owner premise uses two original empty-HPT snapshots with a
-- shared NameCache. Rehydrating the populated result may legitimately reuse HMI.
verifyHydratedOriginalOwners :: IO ()
verifyHydratedOriginalOwners = do
  parent <- getTemporaryDirectory
  bracket (newFixtureDirectory parent) removePathForcibly $ \work -> do
    let path = work </> "HydratedOwnerFixture.hs"
        writeSource value = writeFile path (unlines
          ["module HydratedOwnerFixture where", "__result :: Int", "__result = " ++ show value])
        name = mkModuleName "HydratedOwnerFixture"
    writeSource (41 :: Int)
    withResidentCompilerScopes [work] $ \runScope -> do
      (previous,firstBody) <- runScope (pure ()) $ \scope -> do
        prepared <- scopedCompile scope (PreparedProducts Nothing) Set.empty GeneralCompile Nothing path [work] Nothing
        original <- maybe (fail "actual compiler omitted finalization") pure (Map.lookup name (pprFinalizedModules prepared))
        let environment = prHscEnv (pprPipelineResult prepared)
            actualHome = finalizedHomeModInfo original
        body <- captureFinalizedInterface original
        let artifact = ExactIfaceArtifact "main" "HydratedOwnerFixture" "selected/original.hi"
              (artifactSha256 body) (homeInterfaceUsageOwners environment (hm_iface actualHome))
            alias = artifact {exactPath="receiving/alias.hi"}
        decoder <- newRequestIfaceDecoder
        capturedRoles <- forM [artifact,alias] $ \selected ->
          readCapturedExactIfaceClosureWithCheckedValues decoder
            (\requested -> if requested == selected then pure body else fail "unexpected original owner")
            environment [selected] [] >>= either fail pure
        captured <- either throwIO pure (mergeCapturedInterfaceInputs capturedRoles)
        emptyA <- freshExactContext environment
        emptyB <- freshExactContext environment
        first <- hydrateInterfaceInputs emptyA captured
        second <- hydrateInterfaceInputs emptyB captured
        let closure = hydratedExactInputs first
            retained = hydratedClosureAsCapturedInputs closure
        _ <- either throwIO pure (mergeCapturedInterfaceInputs [retained,retained])
        assert (case mergeCapturedInterfaceInputs
            [retained,hydratedClosureAsCapturedInputs (hydratedExactInputs second)] of
              Left InterfaceCompletedOwnerConflict{} -> True; _ -> False)
          "independent equal-byte original allocations silently coalesced"
        repeated <- hydrateInterfaceInputs (hydratedExactEnvironment first) captured
        _ <- either throwIO pure (mergeCapturedInterfaceInputs
          [retained,hydratedClosureAsCapturedInputs (hydratedExactInputs repeated)])
        selected <- either throwIO pure (selectHydratedInterfaceInputs [alias] closure)
        fresh <- freshExactContext environment
        installed <- retainInstalledInterfaceInputs fresh selected
        firstIdentity <- case hydratedExactInterfaces first of
          [(_,home)] -> evaluate home >>= makeStableName
          _ -> fail "original hydration changed its selected owner set"
        case hydratedExactInterfaces installed of
          [(_,home)] -> do
            identity <- evaluate home >>= makeStableName
            assert (identity == firstIdentity) "alias retention replaced the actual original HMI"
          _ -> fail "alias retention changed its selected owner set"
        _ <- either fail pure (selectVerifiedExactInterfaceProducts
          (hydratedClosureAsCapturedInputs (hydratedExactInputs installed)) [alias])
        finalized <- retainCompletedModuleInterface environment original actualHome
        completed <- captureCompletedInterfaceInputs environment closure [] [finalized]
        completionOwner <- retainInstalledInterfaceInputs fresh completed
        expectedIdentity <- evaluate actualHome >>= makeStableName
        case hydratedExactInterfaces completionOwner of
          [(_,home)] -> do
            identity <- evaluate home >>= makeStableName
            assert (identity == expectedIdentity) "finalization did not establish its completed allocation"
          _ -> fail "finalization changed its selected owner set"
        _ <- either fail pure (selectVerifiedExactInterfaceProducts (hydratedClosureAsCapturedInputs completed) [alias])
        pure (closure,artifactSha256 body)
      writeSource (42 :: Int)
      runScope (pure ()) $ \scope -> do
        changed <- scopedCompile scope (PreparedProducts Nothing) Set.empty GeneralCompile Nothing path [work] Nothing
        original <- maybe (fail "changed compiler omitted finalization") pure (Map.lookup name (pprFinalizedModules changed))
        let environment = prHscEnv (pprPipelineResult changed)
        body <- captureFinalizedInterface original
        assert (artifactSha256 body /= firstBody) "changed-byte refusal premise did not change the interface"
        finalized <- retainCompletedModuleInterface environment original (finalizedHomeModInfo original)
        refused <- try (captureCompletedInterfaceInputs environment previous [] [finalized])
        assert (case refused of Left (InterfaceInputConflict key) -> key == ("main","HydratedOwnerFixture"); _ -> False)
          "finalization retained an old alias for changed interface bytes"

verifyCompletedInterfaceMerges :: FilePath -> FilePath -> HscEnv -> IO ()
verifyCompletedInterfaceMerges libdir work environment = do
  let selected = SessionModule ValMod (Generation 991337)
  interface <- mkThinSessionIface environment selected [(mkVarOcc "value",intTy)]
  writeSessionIface environment work selected interface
  body <- captureArtifactBytes <$> BS.readFile (sessionHiPath work selected)
  root <- packageImportRoot environment (nameModule (tyConName intTyCon)) >>= either fail pure
  let artifact = ExactIfaceArtifact "main" (sessionModuleString selected) (sessionHiPath work selected)
        (artifactSha256 body) []
      packages = captureArtifactBytes (encodePackageImports artifact (PackageImportEvidence [root] []))
      changedPackages = captureArtifactBytes (encodePackageImports artifact (PackageImportEvidence [] []))
  capturedInput <- readVerifiedExactIfaceClosure environment [artifact] >>= either fail pure
  admittedValues <- mapM (either fail pure . admitCheckedValueInterface packages) (verifiedInterfaceProducts capturedInput)
  admitted <- either throwIO pure (capturedInterfaceInputs admittedValues)
  injectedOwner <- hydrateInterfaceInputs environment admitted
  let injected = hydratedExactEnvironment injectedOwner
  capture <- either throwIO pure (capturedSessionInterfaceFor
    (hydratedClosureAsCapturedInputs (hydratedExactInputs injectedOwner)) artifact body packages)
    >>= maybe (fail "authenticated hydration lost its session issuer") pure
  let (_,actualHome) = capturedSessionCompilerProduct capture
  checked <- either throwIO pure (checkCapturedSessionInterface artifact body packages capture)
  assert (sameCapturedSessionInterfaceOwner capture checked)
    "selected sidecar check replaced the actual completed issuer"
  forM_ [(artifact,changedPackages),(artifact {exactRequirements=[("main","AbsentNominalOwner")]},packages)] $
    \(selectedArtifact,selectedPackages) ->
      assert (case checkCapturedSessionInterface selectedArtifact body selectedPackages capture of
        Left InterfaceSessionAliasMismatch{} -> True; _ -> False)
        "completed issuer reuse ignored a changed package token or nominal requirement"
  (_,_,bareCapture) <- injectSessionIfaceWithBindings work selected environment
  assert (case checkCapturedSessionInterface artifact body packages bareCapture of
      Left InterfaceSessionAliasMismatch{} -> True; _ -> False)
    "completed issuer reuse accepted absent package evidence"
  let empty = emptyHydratedInterfaceClosure
  completed <- captureCompletedInterfaceInputs injected empty [(artifact,capture)] []
  decoded <- readVerifiedExactIfaceClosure environment [artifact] >>= either fail pure
  decoder <- newRequestIfaceDecoder
  let alias = artifact {exactPath="primary-4/iface"}
  aliased <- readCapturedExactIfaceClosureWithCheckedValues decoder
    (\requested -> if requested == alias then pure body else fail "unexpected receiving alias")
    environment [] [alias] >>= either fail pure
  forM_ [[hydratedClosureAsCapturedInputs completed,aliased],[aliased,hydratedClosureAsCapturedInputs completed]] $ \inputs -> do
    merged <- either throwIO pure (mergeCapturedInterfaceInputs inputs)
    retained <- hydratedClosureAsCapturedInputs . hydratedExactInputs <$> hydrateInterfaceInputs injected merged
    borrowed <- either throwIO pure (capturedSessionInterfaceFor retained alias body packages)
    assert (maybe False (sameCapturedSessionInterfaceOwner capture) borrowed)
      "completed input retention lost an authenticated logical receiving alias"
  aliasedInputs <- either throwIO pure (mergeCapturedInterfaceInputs
    [hydratedClosureAsCapturedInputs (hydratedExactInputs injectedOwner),aliased])
  aliasedOwner <- hydrateInterfaceInputs injected aliasedInputs
  selectedAlias <- either throwIO pure (selectHydratedInterfaceInputs [alias] (hydratedExactInputs aliasedOwner))
  aliasPhase <- freshExactContext environment
  aliasOwner <- retainInstalledInterfaceInputs aliasPhase selectedAlias
  aliasCapture <- either throwIO pure (capturedSessionInterfaceFor
    (hydratedClosureAsCapturedInputs (hydratedExactInputs aliasOwner)) alias body packages)
  assert (maybe False (sameCapturedSessionInterfaceOwner capture) aliasCapture)
    "checked hydration/selection/retention dropped its receiving alias"
  actualIdentity <- evaluate actualHome >>= makeStableName
  absentCompleted <- freshExactContext injected
  retainedCompleted <- retainInstalledInterfaceInputs absentCompleted completed
  case hydratedExactInterfaces retainedCompleted of
    [(selectedArtifact,retainedHome)] -> do
      assert (selectedArtifact == artifact) "retain-only completed selection changed its descriptor"
      retainedIdentity <- evaluate retainedHome >>= makeStableName
      assert (retainedIdentity == actualIdentity)
        "retain-only completion reconstructed its actual HMI when absent from the phase"
    _ -> fail "retain-only completion changed the selected owner set"
  thinOwner <- hydrateInterfaceInputs injected decoded
  thinRefusal <- try (retainInstalledInterfaceInputs injected (hydratedExactInputs thinOwner) >> pure ())
    :: IO (Either InterfaceInputFailure ())
  assert (case thinRefusal of
      Left (InterfaceUnownedThinHome owner) -> owner == ("main",sessionModuleString selected)
      _ -> False)
    "retain-only decoded thin input borrowed an unowned zero-fingerprint HMI"
  forM_ [[hydratedClosureAsCapturedInputs completed,decoded],[decoded,hydratedClosureAsCapturedInputs completed]] $ \inputs -> do
    merged <- either throwIO pure (mergeCapturedInterfaceInputs inputs)
    reused <- either throwIO pure (capturedSessionInterfaceFor merged artifact body packages)
    assert (maybe False (sameCapturedSessionInterfaceOwner capture) reused)
      "another selected role lost its actual completed session issuer"
    fresh <- freshExactContext environment
    owner <- hydrateInterfaceInputs fresh merged
    retained <- maybe (fail "completed interface merge lost its HMI") pure
      (lookupHpt (hsc_HPT (hydratedExactEnvironment owner)) (mkModuleName (sessionModuleString selected)))
    retainedIdentity <- evaluate retained >>= makeStableName
    assert (retainedIdentity == actualIdentity)
      "captured merge replaced the actual completed HMI with a decoded reconstruction"
  fresh <- freshExactContext environment
  decodedOwner <- hydrateInterfaceInputs fresh decoded
  decodedHome <- maybe (fail "raw decoded interface failed hydration") pure
    (lookupHpt (hsc_HPT (hydratedExactEnvironment decodedOwner)) (mkModuleName (sessionModuleString selected)))
  decodedIdentity <- evaluate decodedHome >>= makeStableName
  assert (decodedIdentity /= actualIdentity) "raw decoded input silently claimed a completed HMI owner"
  completedOverDecoded <- retainInstalledInterfaceInputs (hydratedExactEnvironment decodedOwner) completed
  case hydratedExactInterfaces completedOverDecoded of
    [(selectedArtifact,retainedHome)] -> do
      assert (selectedArtifact == artifact) "completed retention changed its selected descriptor"
      retainedIdentity <- evaluate retainedHome >>= makeStableName
      assert (retainedIdentity == actualIdentity && retainedIdentity /= decodedIdentity)
        "equal interface bytes replaced the completed HMI with another allocation"
    _ -> fail "completed retention over a decoded HMI changed its selected owners"
  foreignEnvironment <- runGhc (Just libdir) getSession
  foreignRetain <- try (retainInstalledInterfaceInputs foreignEnvironment completed >> pure ())
    :: IO (Either InterfaceInputFailure ())
  assert (case foreignRetain of Left InterfaceNameCacheMismatch -> True; _ -> False)
    "retain-only completion accepted another NameCache"
  foreignInput <- readVerifiedExactIfaceClosure foreignEnvironment [artifact] >>= either fail pure
  assert (case mergeCapturedInterfaceInputs [hydratedClosureAsCapturedInputs completed,foreignInput] of
      Left InterfaceNameCacheMismatch -> True; _ -> False)
    "captured merge accepted an interface from another NameCache"
  secondEnvironment <- freshExactContext environment
  otherOwner <- hydrateInterfaceInputs secondEnvironment admitted
  otherCapture <- either throwIO pure (capturedSessionInterfaceFor
    (hydratedClosureAsCapturedInputs (hydratedExactInputs otherOwner)) artifact body packages)
    >>= maybe (fail "independent checked hydration lost its session issuer") pure
  otherCompleted <- captureCompletedInterfaceInputs injected empty [(artifact,otherCapture)] []
  assert (case mergeCapturedInterfaceInputs (map hydratedClosureAsCapturedInputs [completed,otherCompleted]) of
      Left InterfaceCompletedOwnerConflict{} -> True; _ -> False)
    "equal interface bytes silently selected another completed owner"
  _ <- newOriginalInterfaceArtifactsWithSessionCaptures injected Map.empty [] [capture,capture] work
  competing <- try (newOriginalInterfaceArtifactsWithSessionCaptures injected Map.empty []
    [capture,otherCapture] work >> pure ()) :: IO (Either SomeException ())
  assert (case competing of Left _ -> True; Right () -> False)
    "original artifact retention selected between competing completed session issuers"

-- The resource owns only successful producer outputs. Module-local flags keep
-- Core visibility and optimizer metadata independent of the consumer profile.
data ProducedFixtures = ProducedFixtures
  { fixtureDirectory :: FilePath
  , fixtureLibdir :: FilePath
  , fixtureBoot :: HscEnv
  , fixtureHomes :: HomePackageTable
  , fixtureThinBody :: ArtifactBytes
  }

data PrivateNames = PrivateNames Name Name Name Name Name Name
-- Constructors stay in this fixture owner. Consumers require the checked role,
-- rather than accepting a generic environment that might have another profile.
data PrivateOriginalFixture = PrivateOriginalFixture ProducedFixtures PrivateNames [CoreBind]
data ThinTypeFixture = ThinTypeFixture ProducedFixtures ExactIfaceArtifact Id
data InlineOriginalFixture = InlineOriginalFixture ProducedFixtures Id

acquireProducedFixtures :: IO ProducedFixtures
acquireProducedFixtures = do
  parent <- getTemporaryDirectory
  work <- newFixtureDirectory parent
  produce work `onException` removePathForcibly work
  where
    produce work = do
      let sources = "test-prepared-stg" </> "fat-iface-fixtures"
      mapM_ (copyFixture sources work)
        ["FatFixture.hs", "ThinFixture.hs", "MissingFixture.hs", "FatIfaceUse.hs"]
      libdir <- getLibdir
      runGhc (Just libdir) $ do
        flags <- getSessionDynFlags
        let consumerFlags = (gopt_unset (updOptLevel 0 flags) Opt_IgnoreInterfacePragmas)
              { importPaths=work:importPaths flags, hiDir=Just work, objectDir=Just work }
        _ <- setSessionDynFlags consumerFlags
        target <- guessTarget (work </> "FatIfaceUse.hs") Nothing Nothing
        setTargets [target]
        loaded <- load LoadAllTargets
        case loaded of
          Failed -> fail "fixture producer failed its actual GHC load"
          Succeeded -> pure ()
        actual <- getSession
        let homes = filterHpt (\home -> moduleNameString (moduleName (mi_module (hm_iface home)))
              `elem` ["FatFixture", "ThinFixture", "MissingFixture"]) (hsc_HPT actual)
        liftIO $ forM_ ["FatFixture", "ThinFixture", "MissingFixture"] $ \name ->
          unless (maybe False ((== mkModuleName name) . moduleName . mi_module . hm_iface)
              (lookupHpt homes (mkModuleName name)))
            (fail ("successful fixture producer omitted " ++ name))
        body <- liftIO (BS.readFile (work </> "ThinFixture.hi") >>= evaluate . captureArtifactBytes)
        boot <- liftIO (freshExactContext actual)
        pure (ProducedFixtures work libdir boot homes body)

releaseProducedFixtures :: ProducedFixtures -> IO ()
releaseProducedFixtures = removePathForcibly . fixtureDirectory

-- Share immutable producer HMI/byte owners and their NameCache; each consumer
-- receives independent package/finder cells and creates its own mutable caches.
freshFixtureEnvironment :: ProducedFixtures -> IO HscEnv
freshFixtureEnvironment fixture = do
  fresh <- freshExactContext (fixtureBoot fixture)
  pure (hscUpdateHPT (const (fixtureHomes fixture)) fresh)

fixtureInterface :: HscEnv -> String -> IO ModIface
fixtureInterface environment module' = maybe
  (fail ("fixture producer has no actual HMI for " ++ module')) (pure . hm_iface)
  (lookupHpt (hsc_HPT environment) (mkModuleName module'))

fixtureExport :: ModIface -> String -> IO Name
fixtureExport iface occurrence = case
    [name | name <- concatMap availNames (mi_exports iface)
      , nameModule_maybe name == Just (mi_module iface)
      , occNameString (nameOccName name) == occurrence] of
  [name] -> pure name
  names -> fail ("fixture export " ++ occurrence ++ " has " ++ show (length names) ++ " actual Names")

-- Upstream GHC decoding supplies the premise independently of Tidepool's
-- private-component index and selected-body closure.
fixtureWholeCore :: HscEnv -> ModIface -> IO [CoreBind]
fixtureWholeCore environment iface = do
  encoded <- maybe (fail "private fixture producer omitted defining Core") pure (mi_extra_decls iface)
  let doc = text "fat interface fixture premise"
  initIfaceCheck doc environment $ do
    types <- liftIO (newIORef emptyTypeEnv)
    initIfaceLcl (mi_module iface) doc NotBoot (tcTopIfaceBindings types encoded)

fixtureInlineId :: HscEnv -> ModIface -> String -> IO Id
fixtureInlineId environment iface occurrence = do
  name <- fixtureExport iface occurrence
  details <- initIfaceCheck (text "INLINE fixture premise") environment (typecheckIface iface)
  identifier <- case [value | value <- typeEnvIds (md_types details),varName value == name] of
    [value] -> pure value
    _ -> fail ("actual INLINE fixture omitted its defining Id: " ++ occurrence)
  requireInlineEnvelope occurrence identifier
  pure identifier

requireInlineEnvelope :: String -> Id -> IO ()
requireInlineEnvelope label identifier = case maybeUnfoldingTemplate (realIdUnfolding identifier) of
  Just _ -> pure ()
  Nothing -> fail (label ++ " producer omitted its genuine optimizer INLINE envelope")

checkPrivateFixture :: ProducedFixtures -> IO PrivateOriginalFixture
checkPrivateFixture fixture = do
  environment <- freshFixtureEnvironment fixture
  iface <- fixtureInterface environment "FatFixture"
  names <- PrivateNames <$> fixtureExport iface "fatIdentity" <*> fixtureExport iface "privateCaller"
    <*> fixtureExport iface "privateDiamond" <*> fixtureExport iface "foreignAbs"
    <*> fixtureExport iface "recA" <*> fixtureExport iface "recB"
  whole <- fixtureWholeCore environment iface
  let PrivateNames _ caller _ _ _ _ = names
      binders = concatMap groupBinders whole
      helpers = [value | value <- binders,occNameString (nameOccName (varName value)) == "privateHelper"]
      diagnostic = "actual whole-Core binders=" ++ show (map binderPremise binders)
  helper <- case helpers of
    [value] -> pure value
    _ -> fail ("private producer requires exactly one privateHelper; " ++ diagnostic)
  assert (not (isExternalName (varName helper)))
    ("private producer helper has an External Name; " ++ diagnostic)
  assert (not (isLocalId helper))
    ("private producer helper is not a decoded GlobalId; " ++ diagnostic)
  let callerBodies = [body | group <- whole,(identifier,body) <- groupPairs group,varName identifier == caller]
      references = [varName free | body <- callerBodies
        , free <- nonDetEltsUniqSet (exprSomeFreeVars isId body)]
  assert (varName helper `elem` references)
    ("private producer caller has no actual RHS edge to its helper; " ++ diagnostic)
  pure (PrivateOriginalFixture fixture names whole)

binderPremise :: Id -> (String,Bool,Bool)
binderPremise identifier = (occNameString (nameOccName (varName identifier)),
  isExternalName (varName identifier),isLocalId identifier)

groupPairs :: CoreBind -> [(Id,CoreExpr)]
groupPairs (NonRec identifier body) = [(identifier,body)]
groupPairs (Rec pairs) = pairs

admitThinFixture :: ProducedFixtures -> HscEnv -> RequestIfaceDecoder -> String
  -> IO (ExactIfaceArtifact,CapturedInterfaceInputs)
admitThinFixture fixture environment decoder alias = do
  home <- maybe (fail "actual thin producer HMI is absent") pure
    (lookupHpt (fixtureHomes fixture) (mkModuleName "ThinFixture"))
  let artifact = ExactIfaceArtifact (unitString (moduleUnit (mi_module (hm_iface home)))) "ThinFixture" alias
        (artifactSha256 (fixtureThinBody fixture)) []
  inputs <- readCapturedExactIfaceClosureWithCheckedValues decoder
    (\requested -> if requested == artifact then pure (fixtureThinBody fixture) else fail "unexpected captured fixture input")
    environment [artifact] [] >>= either fail pure
  products <- either fail pure (selectVerifiedExactInterfaceProducts inputs [artifact])
  assert (length products == 1 && all
      (maybe True (const False) . mi_extra_decls . snd . verifiedExactInterface) products)
    "the actual admitted thin fixture contains defining Core or lost its selected owner"
  pure (artifact,inputs)

checkThinFixture :: ProducedFixtures -> IO ThinTypeFixture
checkThinFixture fixture = do
  environment <- freshFixtureEnvironment fixture
  decoder <- newRequestIfaceDecoder
  (artifact,inputs) <- admitThinFixture fixture environment decoder "primary-1/iface"
  fresh <- freshExactContext environment
  hydrated <- hydrateInterfaceInputs fresh inputs
  iface <- fixtureInterface (hydratedExactEnvironment hydrated) "ThinFixture"
  identifier <- fixtureInlineId (hydratedExactEnvironment hydrated) iface "thinIdentity"
  pure (ThinTypeFixture fixture artifact identifier)

checkInlineFixture :: ProducedFixtures -> IO InlineOriginalFixture
checkInlineFixture fixture = do
  environment <- freshFixtureEnvironment fixture
  iface <- fixtureInterface environment "MissingFixture"
  assert (maybe False (const True) (mi_extra_decls iface))
    "public INLINE producer omitted its defining Core"
  identifier <- fixtureInlineId environment iface "missingIdentity"
  pure (InlineOriginalFixture fixture identifier)

withFixtureFileRemoved :: ProducedFixtures -> String -> IO a -> IO a
withFixtureFileRemoved fixture module' action = do
  let path = fixtureDirectory fixture </> module' ++ ".hi"
  bytes <- BS.readFile path
  bracket (removeFile path) (\() -> BS.writeFile path bytes) (const action)

verifyCapturedHome :: PrivateOriginalFixture -> IO ()
verifyCapturedHome (PrivateOriginalFixture fixture (PrivateNames _ privateCallerName _ _ _ _) _) = do
  environment <- freshFixtureEnvironment fixture
  original <- fixtureInterface environment "FatFixture"
  packageRead <- readPackageInterface environment (mi_module original)
  assert (case packageRead of Left _ -> True; Right _ -> False)
    "package interface reader accepted a HOME input"
  absent <- freshExactContext environment
  absentCache <- newFatIfaceCache
  missing <- lookupFatIfaceExact absent absentCache privateCallerName
  assert (case missing of FatIfaceLoadFailure{} -> True; _ -> False)
    "missing HOME owner reopened an existing physical interface"
  withFixtureFileRemoved fixture "FatFixture" $ do
    _ <- foldM (\current alias -> do
      let artifact = ExactIfaceArtifact (unitString (moduleUnit (mi_module original))) "FatFixture" alias "" []
      hydrated <- hydrateExactScope current [(artifact,original)]
      resolved <- readHomeInterface hydrated (mi_module original)
      assert (case resolved of Right iface -> mi_module iface == mi_module original; Left _ -> False)
        "retained HOME interface lost its owner"
      isolated <- newFatIfaceCache
      recovered <- lookupFatIfaceExact hydrated isolated privateCallerName
      assertPrivateScope privateCallerName recovered
      pure hydrated) environment ["primary-0/iface", "primary-0/iface", "primary-7/iface"]
    pure ()

verifyCapturedThin :: ThinTypeFixture -> IO ()
verifyCapturedThin (ThinTypeFixture fixture _ identifier) = do
  environment <- freshFixtureEnvironment fixture
  decoder <- newRequestIfaceDecoder
  withFixtureFileRemoved fixture "ThinFixture" $ do
    let capture selected alias = snd <$> admitThinFixture fixture selected decoder alias
    forM_ ["primary-1/iface", "primary-1/iface", "primary-9/iface"] $ \alias -> do
      inputs <- capture environment alias
      fresh <- freshExactContext environment
      hydrated <- hydrateInterfaceInputs fresh inputs
      cache <- newFatIfaceCache
      result <- lookupFatIfaceExact (hydratedExactEnvironment hydrated) cache (varName identifier)
      assert (isNoExtra result) "captured TYPE attempted a defining-file read"
    first <- capture environment "primary-1/iface"
    other <- runGhc (Just (fixtureLibdir fixture)) $ do
      flags <- getSessionDynFlags
      _ <- setSessionDynFlags flags
      getSession
    _ <- capture other "primary-1/iface"
    repeated <- capture environment "primary-1/iface"
    let objects inputs = mapM (\verified -> evaluate (snd (verifiedExactInterface verified)) >>= makeStableName)
          (verifiedInterfaceProducts inputs)
    initialObjects <- objects first
    repeatedObjects <- objects repeated
    assert (initialObjects == repeatedObjects)
      "interleaving another NameCache discarded the actual decoded owner"
    crossed <- try (hydrateInterfaceInputs other first >> pure ()) :: IO (Either SomeException ())
    assert (case crossed of Left _ -> True; Right () -> False)
      "TYPE hydration accepted another NameCache"
    fresh <- freshExactContext environment
    installed <- hydrateInterfaceInputs fresh first
    foreignPhase <- try (restoreHydratedInterfaceEnvironment installed other >> pure ()) :: IO (Either SomeException ())
    assert (case foreignPhase of Left _ -> True; Right () -> False)
      "phase restoration accepted another NameCache"

verifyThinInlineRefusal :: ThinTypeFixture -> IO ()
verifyThinInlineRefusal (ThinTypeFixture fixture artifact identifier) = do
  environment <- freshFixtureEnvironment fixture
  decoder <- newRequestIfaceDecoder
  (_,inputs) <- admitThinFixture fixture environment decoder (exactPath artifact)
  fresh <- freshExactContext environment
  hydrated <- hydrateInterfaceInputs fresh inputs
  let selected = hydratedExactEnvironment hydrated
  genuine <- declaringId selected identifier
  cache <- newFatIfaceCache
  result <- lookupFatIfaceExact selected cache (varName identifier)
  assert (isNoExtra result) "thin interface was confused with an absent binding"
  repeated <- lookupFatIfaceExact selected cache (missingNameIn (varName identifier))
  assert (isNoExtra repeated) "typed no-extra outcome was not retained"
  recovered <- recoverExactBody selected cache genuine
  case recovered of
    MissingExactBody name NoExtraDeclarations -> assert (name == varName identifier)
      "thin recovery refusal changed the exact defining identity"
    _ -> fail "optimizer unfolding bypassed missing defining Core"

verifyRetainedHome :: InlineOriginalFixture -> IO ()
verifyRetainedHome (InlineOriginalFixture fixture identifier) = do
  environment <- freshFixtureEnvironment fixture
  genuine <- declaringId environment identifier
  let path = fixtureDirectory fixture </> "MissingFixture.hi"
  original <- fixtureInterface environment "MissingFixture"
  -- Admit the production TYPE projection, not the defining Core retained by
  -- this fixture's physical fat interface.
  body <- captureArtifactBytes <$> serializeOriginalInterface environment
    (fixtureDirectory fixture) (set_mi_extra_decls Nothing original)
  let artifact = ExactIfaceArtifact (unitString (moduleUnit (mi_module original)))
        "MissingFixture" path (artifactSha256 body) []
  decoder <- newRequestIfaceDecoder
  captured <- readCapturedExactIfaceClosureWithCheckedValues decoder
    (\requested -> if requested == artifact then pure body else fail "unexpected retained TYPE input")
    environment [artifact] [] >>= either fail pure
  assert (all (maybe True (const False) . mi_extra_decls . snd . verifiedExactInterface)
      (verifiedInterfaceProducts captured))
    "retained TYPE projection still contains defining Core"
  actualHome <- maybe (fail "producer lost MissingFixture HMI") pure
    (lookupHpt (hsc_HPT environment) (mkModuleName "MissingFixture"))
  actualIdentity <- evaluate actualHome >>= makeStableName
  assert (maybe False (const True) (lookupHpt (hsc_HPT environment) (mkModuleName "FatFixture")))
    "retain-only exclusion premise lacks an unrelated actual HPT owner"
  hydrated <- hydrateInterfaceInputs environment captured
  withFixtureFileRemoved fixture "MissingFixture" $ do
    installed <- retainInstalledInterfaceInputs environment (hydratedExactInputs hydrated)
    case hydratedExactInterfaces installed of
      [(selectedArtifact,retainedHome)] -> do
        assert (selectedArtifact == artifact)
          "retain-only TYPE selection changed its descriptor or included unrelated HPT owners"
        retainedIdentity <- evaluate retainedHome >>= makeStableName
        assert (retainedIdentity == actualIdentity) "retain-only TYPE rebuilt its existing HMI"
      _ -> fail "retain-only TYPE included unrelated HPT owners"
    missingPhase <- freshExactContext environment
    missing <- retainInstalledInterfaceInputs missingPhase (hydratedExactInputs hydrated)
    case hydratedExactInterfaces missing of
      [(_,home)] -> do
        identity <- evaluate home >>= makeStableName
        assert (identity == actualIdentity) "retention into an empty HPT replaced the actual hydrated allocation"
      _ -> fail "retention into an empty HPT lost the hydrated owner"
    retainedCache <- newFatIfaceCache
    retained <- lookupFatIfaceExact environment retainedCache (varName identifier)
    assert (isFound retained) "unlink invalidated its retained HOME interface owner"
    absent <- freshExactContext environment
    cache <- newFatIfaceCache
    result <- lookupFatIfaceExact absent cache (varName identifier)
    assertLoadFailure "MissingFixture" result
    repeated <- lookupFatIfaceExact absent cache (varName identifier)
    assertLoadFailure "MissingFixture" repeated
    recovered <- recoverExactBody absent cache genuine
    case recovered of
      BodyInterfaceFailure owner reason -> do
        assert (Just owner == nameModule_maybe (varName identifier)) "refusal changed its defining owner"
        assert (not (null reason)) "HOME owner refusal lost its reason"
      _ -> fail "optimizer unfolding bypassed the absent HOME owner"

verifyCompletedCaptures :: IO ()
verifyCompletedCaptures = do
  libdir <- getLibdir
  parent <- getTemporaryDirectory
  withTempDirectory parent $ \work -> do
    environment <- runGhc (Just libdir) $ do
      flags <- getSessionDynFlags
      _ <- setSessionDynFlags flags
      getSession
    verifyCompletedInterfaceMerges libdir work environment

verifyPrivateClosure :: PrivateOriginalFixture -> IO ()
verifyPrivateClosure (PrivateOriginalFixture fixture (PrivateNames fatIdentityName privateCallerName _ foreignAbsName recAName recBName) _) = do
  hsc <- freshFixtureEnvironment fixture
  cache <- newFatIfaceCache
  localResult <- lookupFatIfaceExact hsc cache
    (mkSystemName (mkUnique 'v' 983450) (mkVarOcc "localOnly"))
  assert (isNameWithoutModule localResult)
    "name without a module was not reported as typed absence"
  fatIdentityResult <- lookupFatIfaceExact hsc cache fatIdentityName
  assert (isFound fatIdentityResult) "fat identity was not recovered"
  privateResult <- lookupFatIfaceExact hsc cache privateCallerName
  assertPrivateScope privateCallerName privateResult
  privateOwner <- maybe (fail "private scope fixture has no owner") pure
    (nameModule_maybe privateCallerName)
  foreignResult <- lookupFatIfaceExact hsc cache foreignAbsName
  case foreignResult of
    FatIfaceFound groups -> do
      let calls = [identifier | group <- groups
            , rhs <- case group of NonRec _ body -> [body]; Rec pairs -> map snd pairs
            , identifier <- nonDetEltsUniqSet (exprSomeFreeVars (\identifier -> isId identifier && isFCallId identifier) rhs)]
      assert (not (null calls) && all (not . isExternalName . varName) calls)
        "compiled FFI fixture did not decode a genuine internal-name operation Id"
      _ <- evaluate (sum (map Set.size (IntMap.elems (privateOriginalDependencies privateOwner (IntMap.fromList (zip [0..] groups))))))
      pure ()
    other -> fail ("compiled FFI original failed admission: " ++ showLookup other)
  privateGroups <- case privateResult of
    FatIfaceFound groups -> pure groups
    _ -> fail "private defining closure disappeared"
  do
    let withoutHelper = [group | group <- privateGroups
          , all ((/= "privateHelper") . occNameString . nameOccName . varName) (groupBinders group)]
    assert (length withoutHelper < length privateGroups) "orphan control did not remove the genuine private group"
    refused <- try (evaluate (sum (map Set.size
      (IntMap.elems (privateOriginalDependencies privateOwner (IntMap.fromList (zip [0..] withoutHelper))))))) :: IO (Either ErrorCall Int)
    case refused of
      Left _ -> pure ()
      Right _ -> fail "private original census accepted a genuinely dangling decoded GlobalId"
  forward <- lookupFatIfaceBodies hsc cache privateOwner [privateCallerName, fatIdentityName]
  reverseRoots <- lookupFatIfaceBodies hsc cache privateOwner
    [fatIdentityName, privateCallerName, privateCallerName]
  case (forward, reverseRoots) of
    (FatIfaceFound{}, FatIfaceFound{}) -> assert (foundNames forward == foundNames reverseRoots)
      "root permutation/duplication changed original group membership or defining order"
    _ -> fail "aggregate private-scope roots lost their defining bodies"
  recAResult <- lookupFatIfaceExact hsc cache recAName
  recBResult <- lookupFatIfaceExact hsc cache recBName
  assertRecGroup ["recA", "recB"] recAResult
  assertRecGroup ["recA", "recB"] recBResult
  absentResult <- lookupFatIfaceExact hsc cache (missingNameIn fatIdentityName)
  assert (isBindingAbsent absentResult)
    "loaded fat interface did not distinguish an absent binding"

verifyComponentSelection :: PrivateOriginalFixture -> IO ()
verifyComponentSelection (PrivateOriginalFixture fixture (PrivateNames _ privateCallerName _ _ _ _) wholeCore) = do
  hsc <- freshFixtureEnvironment fixture
  cache <- newFatIfaceCache
  privateOwner <- maybe (fail "private fixture lost its actual owner") pure (nameModule_maybe privateCallerName)
  componentResult <- lookupFatIfaceComponents hsc cache privateOwner [privateCallerName]
  case componentResult of
    FatIfaceComponents selection -> do
      let components = fatSelectionComponents selection
          census = case components of
            component : _ -> fatComponentAllBinders component
            [] -> []
          componentBinders = concatMap
            (concatMap groupBinders . IntMap.elems . fatComponentBindings) components
          censusNames = map varName census
          componentNames = map varName componentBinders
          selectedGroupCount = sum
            (map (length . fatComponentOrdinals) components)
      assert (fatOriginalOwner (fatSelectionVersion selection) == privateOwner)
        "component selection changed its full defining owner"
      assert (fatSelectionDemandedGroupCount selection ==
          demandedOriginalGroups privateCallerName (zip [0..] wholeCore))
        "component demand count disagreed with actual local Core references"
      assert (fatSelectionPreparedGroupCount selection == selectedGroupCount)
        "prepared group count disagreed with the canonical component roster"
      assert (privateCallerName `elem` censusNames && privateCallerName `elem` componentNames)
        "component selection lost its original actual binder"
      assert (any ((== "privateHelper") . occNameString . nameOccName) censusNames
          && any ((== "privateHelper") . occNameString . nameOccName) componentNames)
        "component selection lost the original private helper binder"
    FatIfaceComponentsMissing reason -> fail ("component selection refused fixture root: " ++ show reason)
    FatIfaceComponentsLoadFailure owner reason -> fail
      ("component selection could not load " ++ moduleNameString (moduleName owner) ++ ": " ++ reason)

verifyPreparedReuse :: PrivateOriginalFixture -> IO ()
verifyPreparedReuse (PrivateOriginalFixture fixture (PrivateNames fatIdentityName privateCallerName privateDiamondName _ _ _) _) = do
  hsc <- freshFixtureEnvironment fixture
  cache <- newFatIfaceCache
  privateOwner <- maybe (fail "private fixture lost its actual owner") pure (nameModule_maybe privateCallerName)
  privateResult <- lookupFatIfaceExact hsc cache privateCallerName
  privateGroups <- case privateResult of
    FatIfaceFound groups -> pure groups
    _ -> fail "checked private fixture lost its selected defining body"
  privateOwners <- newOwnerInterfaceCache
  privateBodies <- newPreparedBodyCache
  privatePrepared <- prepareRecoveredBodies hsc privateOwners privateBodies
    privateOwner privateGroups >>= either (fail . show) pure
  do
    componentCache <- newPreparedBodyCache
    acquireComponents <- newPreparedComponentUnitsPreparer hsc privateOwners componentCache
    let prepare roots = do
          selected <- lookupFatIfaceComponents hsc cache privateOwner roots >>= \case
            FatIfaceComponents selection -> pure selection
            _ -> fail "canonical package growth lost its genuine interface selection"
          task <- acquireComponents Nothing selected >>= either (fail . show) pure
          preparedComponentModules <$> (runPreparedComponentTask task >>= either (fail . show) pure)
        spellings = Map.unions . map pmStableTopSpellings
        physicalBindings prepared = fmap Map.fromList $ fmap concat $ forM (concatMap pmBindings prepared) $ \(binding,_) -> do
          identity <- evaluate binding >>= makeStableName
          pure [(varName binder,identity) | binder <- topBinders binding]
    first <- prepare [privateCallerName]
    firstBindings <- physicalBindings first
    grown <- prepare [privateCallerName,fatIdentityName,privateDiamondName]
    grownBindings <- physicalBindings grown
    assert (all (\(name,identity) -> Map.lookup name grownBindings == Just identity)
      (Map.toList firstBindings))
      "package demand growth re-lowered a completed private component"
    assert (all (\(name,spelling) -> Map.lookup name (spellings grown) == Just spelling)
      (Map.toList (spellings first)))
      "package demand growth renamed an already prepared private top"
    shuffled <- prepare [privateDiamondName,fatIdentityName,privateCallerName,privateCallerName]
    shuffledBindings <- physicalBindings shuffled
    assert (grownBindings == shuffledBindings && spellings grown == spellings shuffled)
      "root permutation or duplication changed canonical prepared component identities"
    bracket (lookupEnv "TIDEPOOL_DISABLE_BODY_REUSE")
      (maybe (unsetEnv "TIDEPOOL_DISABLE_BODY_REUSE") (setEnv "TIDEPOOL_DISABLE_BODY_REUSE")) $ \_ -> do
        setEnv "TIDEPOOL_DISABLE_BODY_REUSE" "1"
        acquireDisabled <- newPreparedComponentUnitsPreparer hsc privateOwners componentCache
        disabledSelection <- lookupFatIfaceComponents hsc cache privateOwner [privateCallerName] >>= \case
          FatIfaceComponents selection -> pure selection
          _ -> fail "body-disable control lost its canonical component"
        disabledTask <- acquireDisabled Nothing disabledSelection >>= either (fail . show) pure
        disabledPrepared <- preparedComponentModules <$> (runPreparedComponentTask disabledTask >>= either (fail . show) pure)
        disabledBindings <- physicalBindings disabledPrepared
        assert (any (\(name,identity) -> Map.lookup name disabledBindings /= Just identity)
          (Map.toList firstBindings))
          "component body-disable calibration returned completed STG instead of lowering"
        assert (Set.fromList (Map.elems (spellings disabledPrepared))
            == Set.fromList (Map.elems (spellings first)))
          "component body-disable calibration changed canonical private spellings"
    selectedCache <- selectPreparedBodyCaches [(componentCache,Set.singleton privateOwner)]
    acquireSelected <- newPreparedComponentUnitsPreparer hsc privateOwners selectedCache
    selected <- lookupFatIfaceComponents hsc cache privateOwner [privateCallerName] >>= \case
      FatIfaceComponents selection -> pure selection
      _ -> fail "selected cache fixture lost its exact component"
    selectedTask <- acquireSelected Nothing selected >>= either (fail . show) pure
    selectedPrepared <- preparedComponentModules <$> (runPreparedComponentTask selectedTask >>= either (fail . show) pure)
    selectedBindings <- physicalBindings selectedPrepared
    assert (selectedBindings == firstBindings)
      "owner-indexed completed cache selection discarded a prepared private unit"
    originalContext <- lookupOwnerInterface privateOwners privateOwner >>= maybe
      (fail "prepared original lacks its declaring context") pure
    independentOwners <- newOwnerInterfaceCache
    independentEnvironment <- forkExactContext hsc
    acquireIndependent <- newPreparedComponentUnitsPreparer independentEnvironment independentOwners componentCache
    independentTask <- acquireIndependent Nothing selected >>= either (fail . show) pure
    independent <- preparedComponentModules <$> (runPreparedComponentTask independentTask >>= either (fail . show) pure)
    independentContext <- lookupOwnerInterface independentOwners privateOwner >>= maybe
      (fail "independently loaded original lacks its declaring context") pure
    assert (not (sameOwnerInterfaceContext originalContext independentContext))
      "independent defining-context issuers aliased the same interface"
    independentBindings <- physicalBindings independent
    assert (any (\(name,identity) -> Map.lookup name independentBindings /= Just identity)
      (Map.toList firstBindings))
      "prepared component cache crossed independently retained declaring contexts"
    independentBodies <- prepareRecoveredBodies independentEnvironment independentOwners privateBodies
      privateOwner privateGroups >>= either (fail . show) pure
    oldBodies <- physicalBindings [privatePrepared]
    newBodies <- physicalBindings [independentBodies]
    assert (any (\(name,identity) -> Map.lookup name newBodies /= Just identity)
      (Map.toList oldBodies))
      "prepared body cache crossed independently retained declaring contexts"
    retainedAgain <- runPreparedComponentTask =<< (acquireSelected Nothing selected >>= either (fail . show) pure)
    retained <- either (fail . show) (physicalBindings . preparedComponentModules) retainedAgain
    assert (retained == firstBindings)
      "another context's completion displaced the retained original context"
  privateOwnerClone <- copyOwnerInterfaceCache privateOwners
  privateOwnerContext <- lookupOwnerInterface privateOwnerClone privateOwner
  assert (maybe False (const True) privateOwnerContext)
    "owner cache copy omitted a completed interface context"
  selectedOwners <- mergeOwnerInterfaceCaches
    [(privateOwners,const False),(privateOwnerClone,(== privateOwner))]
  selectedOwnerContext <- lookupOwnerInterface selectedOwners privateOwner
  assert (maybe False (const True) selectedOwnerContext)
    "owner selection excluded the earlier map but lost a later selected context"
  excludedOwners <- mergeOwnerInterfaceCaches [(privateOwnerClone,const False)]
  excludedOwnerContext <- lookupOwnerInterface excludedOwners privateOwner
  assert (maybe True (const False) excludedOwnerContext)
    "owner merge retained an excluded context"
  indexedOwners <- selectOwnerInterfaceCaches
    [(privateOwners,Set.empty),(privateOwnerClone,Set.singleton privateOwner)]
  indexedOwnerContext <- lookupOwnerInterface indexedOwners privateOwner
  assert (maybe False (const True) indexedOwnerContext)
    "owner-indexed selection lost the requested later context"
  evictOwnerInterfaceMatching privateOwners (== privateOwner)
  clonedOwnerContext <- lookupOwnerInterface privateOwnerClone privateOwner
  assert (maybe False (const True) clonedOwnerContext)
    "evicting the source owner cache changed its independent copy"
  assert (any ((== "privateHelper") . occNameString . nameOccName . varName)
    (concatMap (topBinders . fst) (pmBindings privatePrepared)))
    "native preparation omitted the defining private helper"

-- Generic outcomes exercise loading mechanics without constructing executable
-- Core or allowing tests to insert invented interface facts into FatIfaceCache.
data TestOutcome = NoExtra | LoadFailure String deriving (Eq, Show)

guardTime :: IO a -> IO a
guardTime action = timeout 5000000 action >>= maybe (fail "interface cache test timed out") pure

withLoad :: IO value -> ((ThreadId, MVar (Either SomeException value)) -> IO a) -> IO a
withLoad action = bracket start stop
  where
    start = do
      result <- newEmptyMVar
      thread <- forkFinally action (putMVar result)
      pure (thread, result)
    stop (thread, result) = killThread thread >> guardTime (readMVar result) >> pure ()

awaitLoad :: (ThreadId, MVar (Either SomeException value)) -> IO value
awaitLoad (_, result) = guardTime (readMVar result) >>= either throwIO pure

-- These controlled loaders have no other blocking point. Observing the waiter
-- blocked with one loader entry proves it attached before cancellation/release.
waitBlocked :: ThreadId -> IO ()
waitBlocked thread = guardTime go
  where
    go = threadStatus thread >>= \status -> case status of
      ThreadBlocked BlockedOnMVar -> pure ()
      ThreadFinished -> fail "cache waiter finished before attaching"
      ThreadDied -> fail "cache waiter died before attaching"
      _ -> yield >> go

verifyCacheConcurrency :: IO ()
verifyCacheConcurrency = guardTime $ do
  let owner = 0 :: Int
      other = 1 :: Int
  cache <- newLoadCache
  entered <- newEmptyMVar
  release <- newEmptyMVar
  loads <- newIORef (0 :: Int)
  let loader = do
        atomicModifyIORef' loads (\count -> (count + 1, ()))
        putMVar entered ()
        readMVar release
        pure NoExtra
  withLoad (lookupLoadCache cache owner loader) $ \first -> do
    takeMVar entered
    withLoad (lookupLoadCache cache owner (atomicModifyIORef' loads (\count -> (count + 1, ()))
        >> readMVar release >> pure NoExtra)) $ \second -> do
      waitBlocked (fst second)
      loadCount <- readIORef loads
      assert (loadCount == 1) "same-key callers ran more than one loader"
      independent <- guardTime (lookupLoadCache cache other (pure NoExtra))
      assert (independent == NoExtra) "a blocked owner prevented another key from loading"
      putMVar release ()
      firstResult <- awaitLoad first
      secondResult <- awaitLoad second
      assert (firstResult == NoExtra && secondResult == NoExtra) "same-key completion was not shared"
  clone <- copyLoadCache cache
  cloned <- lookupLoadCache clone owner (fail "completed entry was lost")
  assert (cloned == NoExtra) "cache copy lost its completed entry"
  evictLoadCache cache (== owner)
  independentClone <- lookupLoadCache clone owner (fail "source eviction changed clone")
  assert (independentClone == NoExtra) "source eviction changed the copied cache"

  source <- newLoadCache
  sourceEntered <- newEmptyMVar
  sourceRelease <- newEmptyMVar
  withLoad (lookupLoadCache source owner
      (putMVar sourceEntered () >> readMVar sourceRelease >> pure (LoadFailure "old"))) $ \original -> do
    takeMVar sourceEntered
    inFlightClone <- copyLoadCache source
    copied <- guardTime (lookupLoadCache inFlightClone owner (pure NoExtra))
    assert (copied == NoExtra) "copy inherited an in-flight generation"
    putMVar sourceRelease ()
    outcome <- awaitLoad original
    assert (outcome == LoadFailure "old") "copy prevented the original generation from settling"

  failures <- newLoadCache
  failureLoads <- newIORef (0 :: Int)
  let failing = atomicModifyIORef' failureLoads (\count -> (count + 1, ())) >> pure (LoadFailure "read failed")
  firstFailure <- lookupLoadCache failures owner failing
  nextFailure <- lookupLoadCache failures owner failing
  failureClone <- copyLoadCache failures
  copiedFailure <- lookupLoadCache failureClone owner failing
  count <- readIORef failureLoads
  assert (count == 1 && all (== LoadFailure "read failed") [firstFailure, nextFailure, copiedFailure])
    "cache lost its completed failure outcome"

  cancelled <- newLoadCache
  cancelEntered <- newEmptyMVar
  cancelBlock <- newEmptyMVar
  cancelLoads <- newIORef (0 :: Int)
  withLoad (lookupLoadCache cancelled owner
      (atomicModifyIORef' cancelLoads (\count' -> (count' + 1, ()))
        >> putMVar cancelEntered () >> readMVar cancelBlock >> pure NoExtra)) $ \winner -> do
    takeMVar cancelEntered
    withLoad (lookupLoadCache cancelled owner
        (atomicModifyIORef' cancelLoads (\count' -> (count' + 1, ())) >> pure NoExtra)) $ \waiter -> do
      waitBlocked (fst waiter)
      attached <- readIORef cancelLoads
      assert (attached == 1) "cancellation waiter did not attach to the winning load"
      killThread (fst winner)
      retried <- awaitLoad waiter
      retriedCount <- readIORef cancelLoads
      assert (retried == NoExtra && retriedCount == 2) "winner cancellation did not wake one retry"

  evicted <- newLoadCache
  oldEntered <- newEmptyMVar
  oldRelease <- newEmptyMVar
  newEntered <- newEmptyMVar
  newRelease <- newEmptyMVar
  withLoad (lookupLoadCache evicted owner
      (putMVar oldEntered () >> readMVar oldRelease >> pure (LoadFailure "evicted"))) $ \old -> do
    takeMVar oldEntered
    evictLoadCache evicted (== owner)
    withLoad (lookupLoadCache evicted owner
        (putMVar newEntered () >> readMVar newRelease >> pure NoExtra)) $ \new -> do
      takeMVar newEntered
      putMVar oldRelease ()
      oldResult <- awaitLoad old
      assert (oldResult == LoadFailure "evicted") "evicted winner did not settle its original caller"
      withLoad (lookupLoadCache evicted owner (fail "late old generation replaced newer load")) $ \current -> do
        waitBlocked (fst current)
        putMVar newRelease ()
        newResult <- awaitLoad new
        currentResult <- awaitLoad current
        assert (newResult == NoExtra && currentResult == NoExtra) "late completion replaced a newer generation"

-- Independently enumerate each key's first selected completed source. This
-- checks selection before union, priority and omission without using Map.union
-- or constructing compiler interface/body authority in the test.
verifyCacheMerges :: IO ()
verifyCacheMerges = guardTime $ do
  let keys = [0 :: Int, 1, 2]
      subsets [] = [[]]
      subsets (key : rest) = let tails = subsets rest in tails ++ map (key :) tails
      sources = ["earlier", "later"]
      outcome source key = LoadFailure (source ++ show key)
  caches <- forM sources $ \source -> do
    cache <- newLoadCache
    forM_ keys $ \key -> do
      _ <- lookupLoadCache cache key (pure (outcome source key))
      completed <- lookupCompletedLoadCache cache key
      assert (completed == Just (outcome source key))
        "completed-cache lookup omitted a settled value"
      pure ()
    pure cache
  forM_ (subsets keys) $ \firstSelection -> forM_ (subsets keys) $ \secondSelection -> do
    let selections = [firstSelection,secondSelection]
    merged <- mergeLoadCaches (zip caches (map (flip elem) selections))
    indexed <- selectLoadCaches (zip caches (map Set.fromList selections))
    forM_ keys $ \key -> do
      let expected = case [outcome source key
              | (source,selected) <- zip sources selections, key `elem` selected] of
            found:_ -> found
            [] -> NoExtra
      found <- lookupLoadCache merged key (pure NoExtra)
      indexedFound <- lookupLoadCache indexed key (pure NoExtra)
      assert (found == expected) ("selected cache priority disagreed with first-match oracle: "
        ++ show (key,selections,found,expected))
      assert (indexedFound == expected) ("owner-indexed cache selection disagreed with first-match oracle: "
        ++ show (key,selections,indexedFound,expected))
  empty <- mergeLoadCaches []
  emptyCompleted <- lookupCompletedLoadCache empty (0 :: Int)
  assert (emptyCompleted == Nothing) "completed-cache lookup issued a missing value"
  absent <- lookupLoadCache empty (0 :: Int) (pure NoExtra)
  assert (absent == NoExtra) "empty selection unexpectedly issued an entry"
  completedAfterLoad <- lookupCompletedLoadCache empty 0
  assert (completedAfterLoad == Just NoExtra) "completed-cache lookup lost a newly settled absence"

  pending <- newLoadCache
  complete <- newLoadCache
  entered <- newEmptyMVar
  release <- newEmptyMVar
  _ <- lookupLoadCache complete (0 :: Int) (pure (LoadFailure "later completed"))
  withLoad (lookupLoadCache pending 0 (putMVar entered () >> readMVar release >> pure NoExtra)) $ \running -> do
    takeMVar entered
    pendingCompleted <- lookupCompletedLoadCache pending 0
    completeValue <- lookupCompletedLoadCache complete 0
    assert (pendingCompleted == Nothing) "completed-cache lookup waited on an in-flight value"
    assert (completeValue == Just (LoadFailure "later completed"))
      "completed-cache lookup lost a settled outcome"
    snapshot <- mergeLoadCaches [(pending,const True),(complete,const True)]
    indexedSnapshot <- selectLoadCaches
      [(pending,Set.singleton 0),(complete,Set.singleton 0)]
    found <- guardTime (lookupLoadCache snapshot 0 (fail "merge copied an in-flight cell"))
    assert (found == LoadFailure "later completed") "in-flight earlier entry displaced completed later entry"
    indexedFound <- guardTime (lookupLoadCache indexedSnapshot 0 (fail "indexed selection copied an in-flight cell"))
    assert (indexedFound == LoadFailure "later completed") "indexed loading cell displaced a completed entry"
    putMVar release ()
    _ <- awaitLoad running
    stable <- lookupLoadCache snapshot 0 (fail "late source completion replaced snapshot")
    assert (stable == LoadFailure "later completed") "merged snapshot changed after source settlement"
    indexedStable <- lookupLoadCache indexedSnapshot 0 (fail "late settlement replaced indexed snapshot")
    assert (indexedStable == LoadFailure "later completed") "indexed snapshot changed after source settlement"

-- Use the production defining-interface loader rather than a use-site Id
-- whose optimization metadata the frontend may have omitted.
declaringId :: HscEnv -> Id -> IO Id
declaringId env requested = do
  owner <- maybe (fail "INLINE fixture has no defining owner") pure
    (nameModule_maybe (varName requested))
  owners <- newOwnerInterfaceCache
  bodies <- newPreparedBodyCache
  _ <- prepareRecoveredBodies env owners bodies owner [] >>= either (fail . show) pure
  context <- lookupOwnerInterface owners owner
  original <- case [identifier | Just defining <- [context]
      , identifier <- Map.elems (ownerInterfaceEntries defining)
      , varName identifier == varName requested] of
    [identifier] -> pure identifier
    _ -> fail "INLINE fixture lost its genuine defining Id"
  case maybeUnfoldingTemplate (realIdUnfolding original) of
    Just _ -> pure original
    Nothing -> fail "INLINE defining-interface control has no optimizer unfolding"

copyFixture :: FilePath -> FilePath -> FilePath -> IO ()
copyFixture fixtures work name = readFile (fixtures </> name) >>= writeFile (work </> name)

newFixtureDirectory :: FilePath -> IO FilePath
newFixtureDirectory parent = do
  (path, handle) <- openTempFile parent "tidepool-fat-iface-exact-"
  hClose handle
  removeFile path
  createDirectoryIfMissing True path
  pure path

withTempDirectory :: FilePath -> (FilePath -> IO a) -> IO a
withTempDirectory parent = bracket (newFixtureDirectory parent) removePathForcibly

missingNameIn :: Name -> Name
missingNameIn name = case nameModule_maybe name of
  Just modl -> mkExternalName
    (mkUnique 'v' 983451) modl (mkVarOcc "notInExtraDecls") noSrcSpan
  Nothing -> mkSystemName (mkUnique 'v' 983452) (mkVarOcc "notInExtraDecls")

isFound :: FatIfaceLookup -> Bool
isFound FatIfaceFound{} = True
isFound _ = False

isNoExtra :: FatIfaceLookup -> Bool
isNoExtra (FatIfaceMissing NoExtraDeclarations) = True
isNoExtra _ = False

isBindingAbsent :: FatIfaceLookup -> Bool
isBindingAbsent (FatIfaceMissing BindingAbsent) = True
isBindingAbsent _ = False

isNameWithoutModule :: FatIfaceLookup -> Bool
isNameWithoutModule (FatIfaceMissing NameWithoutModule) = True
isNameWithoutModule _ = False

assertRecGroup :: [String] -> FatIfaceLookup -> IO ()
assertRecGroup expected result = case result of
  FatIfaceFound groups -> do
    let actual = [map (occNameString . nameOccName . varName . fst) pairs | Rec pairs <- groups]
    assert (any (\members -> all (`elem` members) expected) actual)
      ("recursive group omitted a sibling: " ++ show actual)
  other -> ioError (userError ("expected recursive fat binding, got " ++ showLookup other))

-- Private defining tops have internal Names and cannot be discovered by
-- the external recovery worklist. Their exact original groups must travel
-- with the selected body, without admitting unrelated definitions.
assertPrivateScope :: Name -> FatIfaceLookup -> IO ()
assertPrivateScope requested result = case result of
  FatIfaceFound groups -> do
    let identifiers = concatMap groupBinders groups
        helpers = [identifier | identifier <- identifiers,
          occNameString (nameOccName (varName identifier)) == "privateHelper"]
    assert (any ((== requested) . varName) identifiers)
      "private closure omitted its selected exported body"
    let diagnostic = "actual selected binders=" ++ show (map binderPremise identifiers)
    assert (length helpers == 1)
      ("private closure did not retain exactly one privateHelper; " ++ diagnostic)
    assert (all (not . isExternalName . varName) helpers)
      ("private closure helper has an External Name; " ++ diagnostic)
    assert (all (not . isLocalId) helpers)
      ("private closure helper is not a decoded GlobalId; " ++ diagnostic)
    assert (not (any ((== "fatIdentity") . occNameString . nameOccName . varName) identifiers))
      "private closure admitted an unrelated exported group"
  other -> fail ("private defining scope was unavailable: " ++ showLookup other)

foundNames :: FatIfaceLookup -> [[Name]]
foundNames (FatIfaceFound groups) = map (map varName . groupBinders) groups
foundNames _ = []

groupBinders :: CoreBind -> [Id]
groupBinders (NonRec identifier _) = [identifier]
groupBinders (Rec pairs) = map fst pairs

-- Walk the original Core directly using a list of actual binder identities,
-- independently of the production ordinal and weak-component indexes.
demandedOriginalGroups :: Name -> [(Int, CoreBind)] -> Int
demandedOriginalGroups requested groups = Set.size (walk Set.empty roots)
  where
    owners = [(varName binder, ordinal)
      | (ordinal, binding) <- groups, binder <- groupBinders binding]
    roots = [ordinal | (name, ordinal) <- owners, name == requested]
    edges = [(ordinal, target)
      | (ordinal, binding) <- groups
      , rhs <- case binding of NonRec _ body -> [body]; Rec pairs -> map snd pairs
      , free <- nonDetEltsUniqSet (exprSomeFreeVars
          (\identifier -> isId identifier && not (isFCallId identifier) && (isLocalId identifier ||
            (isId identifier && not (isExternalName (varName identifier))))) rhs)
      , Just target <- [lookup (varName free) owners]]
    walk seen [] = seen
    walk seen (ordinal:pending)
      | ordinal `Set.member` seen = walk seen pending
      | otherwise = walk (Set.insert ordinal seen)
          ([target | (source,target) <- edges, source == ordinal] ++ pending)

assertLoadFailure :: String -> FatIfaceLookup -> IO ()
assertLoadFailure wanted result = case result of
  FatIfaceLoadFailure modl reason -> do
    assert (moduleNameString (moduleName modl) == wanted)
      ("load failure named the wrong module: " ++ moduleNameString (moduleName modl))
    assert (not (null reason)) "load failure omitted its reason"
  other -> ioError (userError ("expected typed load failure, got " ++ showLookup other))

showLookup :: FatIfaceLookup -> String
showLookup found@FatIfaceFound{} = "original groups " ++ show (map (map (occNameString . nameOccName)) (foundNames found))
showLookup (FatIfaceMissing missing) = show missing
showLookup (FatIfaceLoadFailure modl reason) =
  moduleNameString (moduleName modl) ++ ": " ++ reason
