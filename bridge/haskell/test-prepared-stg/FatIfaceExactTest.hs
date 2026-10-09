module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Concurrent (ThreadId, forkFinally, killThread, yield)
import Control.Concurrent.MVar
  ( MVar, newEmptyMVar, putMVar, readMVar, takeMVar )
import Control.Exception (SomeException, ErrorCall, try, bracket, finally, throwIO, evaluate)
import Control.Monad (forM, forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.Bits (shiftL, testBit)
import Data.IORef (newIORef, readIORef, atomicModifyIORef')
import qualified Data.IntMap.Strict as IntMap
import Data.List (sortOn)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC
import GHC.Core (CoreBind, Bind(..), maybeUnfoldingTemplate)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Driver.Env (HscEnv)
import GHC.Driver.Session (gopt_set, gopt_unset, updOptLevel)
import GHC.Types.Id (Id, realIdUnfolding, isFCallId)
import GHC.Tc.Types (tcg_rdr_env)
import GHC.Types.Name (mkExternalName, mkSystemName, nameModule_maybe, nameOccName, isExternalName)
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString)
import GHC.Types.Name.Reader (globalRdrEnvElts, greName)
import GHC.Types.Unique (mkUnique)
import GHC.Types.Var (varName, isLocalId, isId)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import System.Directory
  ( createDirectoryIfMissing
  , getTemporaryDirectory
  , removeFile
  , removePathForcibly
  )
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Process (callProcess, readProcess)
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
  , newFatIfaceCache
  , OwnerInterfaceContext, ownerInterfaceLocation, ownerInterfaceTyCons, ownerInterfaceEntries, newOwnerInterfaceCache, copyOwnerInterfaceCache
  , lookupOwnerInterface, sameOwnerInterfaceContext, mergeOwnerInterfaceCaches, selectOwnerInterfaceCaches, evictOwnerInterfaceMatching
  )
import Tidepool.FatIface.Internal
  ( newLoadCache, lookupLoadCache, lookupCompletedLoadCache
  , copyLoadCache, mergeLoadCaches, selectLoadCaches, evictLoadCache
  , privateComponents
  )
import GHC.Conc (ThreadStatus(..), BlockReason(..), threadStatus)
import Tidepool.Resolve (ExactBodyLookup(..), recoverExactBody)
import Tidepool.PreparedStg
  (newPreparedBodyCache, prepareRecoveredBodies, pmBindings, pmStableTopSpellings
  , newPreparedComponentTaskPreparer, runPreparedBodyTask, selectPreparedBodyCaches)
import Tidepool.ExecutionProjection (topBinders)

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "test-prepared-stg"
  [ testCase "exact interface cache lifecycle" scenario
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

scenario :: IO ()
scenario = do
  tmp <- getTemporaryDirectory
  withTempDirectory tmp $ \work -> do
    let fixtures = "test-prepared-stg" </> "fat-iface-fixtures"
        fatSource = work </> "FatFixture.hs"
        thinSource = work </> "ThinFixture.hs"
        missingSource = work </> "MissingFixture.hs"
        useSource = work </> "FatIfaceUse.hs"
    mapM_ (copyFixture fixtures work)
      ["FatFixture.hs", "ThinFixture.hs", "MissingFixture.hs", "FatIfaceUse.hs"]
    let ghc = "ghc"
    compileFixture ghc work ["-fwrite-if-simplified-core"] fatSource
    compileFixture ghc work ["-O1", "-fexpose-all-unfoldings", "-fno-write-if-simplified-core"] thinSource
    compileFixture ghc work ["-O1", "-fexpose-all-unfoldings", "-fwrite-if-simplified-core"] missingSource
    libdir <- trim <$> readProcess ghc ["--print-libdir"] ""
    runGhc (Just libdir) $ do
      flags <- getSessionDynFlags
      let fatFlags =
            (`gopt_set` Opt_WriteInterface)
            $ (`gopt_set` Opt_WriteIfSimplifiedCore)
            $ gopt_unset (updOptLevel 0 flags) Opt_IgnoreInterfacePragmas
      _ <- setSessionDynFlags fatFlags
        { importPaths = work : importPaths flags
        , hiDir = Just work
        , objectDir = Just work
        }
      target <- guessTarget useSource Nothing Nothing
      setTargets [target]
      _ <- load LoadAllTargets
      summary <- getModSummary (mkModuleName "FatIfaceUse")
      parsed <- parseModule summary
      typed <- typecheckModule parsed
      let (tcEnv, _) = tm_internals_ typed
          names = map greName (globalRdrEnvElts (tcg_rdr_env tcEnv))
          fatIdentityName = findName "FatFixture" "fatIdentity" names
          privateCallerName = findName "FatFixture" "privateCaller" names
          privateDiamondName = findName "FatFixture" "privateDiamond" names
          foreignAbsName = findName "FatFixture" "foreignAbs" names
          recAName = findName "FatFixture" "recA" names
          recBName = findName "FatFixture" "recB" names
          thinIdentityName = findName "ThinFixture" "thinIdentity" names
          missingIdentityName = findName "MissingFixture" "missingIdentity" names
      hsc <- getSession
      thinId <- lookupName thinIdentityName >>= \case
        Just (AnId identifier) -> pure identifier
        _ -> fail "thin identity has no genuine exported Id"
      missingId <- lookupName missingIdentityName >>= \case
        Just (AnId identifier) -> pure identifier
        _ -> fail "missing-interface identity has no genuine exported Id"
      cache <- liftIO newFatIfaceCache
      liftIO verifyCacheConcurrency
      localResult <- liftIO (lookupFatIfaceExact hsc cache
        (mkSystemName (mkUnique 'v' 983450) (mkVarOcc "localOnly")))
      liftIO (assert (isNameWithoutModule localResult)
        "name without a module was not reported as typed absence")
      fatIdentityResult <- liftIO (lookupFatIfaceExact hsc cache fatIdentityName)
      liftIO (assert (isFound fatIdentityResult) "fat identity was not recovered")
      privateResult <- liftIO (lookupFatIfaceExact hsc cache privateCallerName)
      liftIO $ assertPrivateScope privateCallerName privateResult
      privateOwner <- maybe (fail "private scope fixture has no owner") pure
        (nameModule_maybe privateCallerName)
      foreignResult <- liftIO (lookupFatIfaceExact hsc cache foreignAbsName)
      liftIO $ case foreignResult of
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
      liftIO $ do
        let withoutHelper = [group | group <- privateGroups
              , all ((/= "privateHelper") . occNameString . nameOccName . varName) (groupBinders group)]
        assert (length withoutHelper < length privateGroups) "orphan control did not remove the genuine private group"
        refused <- try (evaluate (sum (map Set.size
          (IntMap.elems (privateOriginalDependencies privateOwner (IntMap.fromList (zip [0..] withoutHelper))))))) :: IO (Either ErrorCall Int)
        case refused of
          Left _ -> pure ()
          Right _ -> fail "private original census accepted a genuinely dangling decoded GlobalId"
      componentResult <- liftIO (lookupFatIfaceComponents hsc cache privateOwner [privateCallerName])
      liftIO $ case componentResult of
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
              originalGroups = IntMap.toAscList (IntMap.unions
                (map fatComponentBindings components))
          assert (fatOriginalOwner (fatSelectionVersion selection) == privateOwner)
            "component selection changed its full defining owner"
          assert (fatSelectionDemandedGroupCount selection ==
              demandedOriginalGroups privateCallerName originalGroups)
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
      privateOwners <- liftIO newOwnerInterfaceCache
      privateBodies <- liftIO newPreparedBodyCache
      privatePrepared <- liftIO $ prepareRecoveredBodies hsc privateOwners privateBodies
        privateOwner privateGroups >>= either (fail . show) pure
      liftIO $ do
        componentCache <- newPreparedBodyCache
        acquireComponents <- newPreparedComponentTaskPreparer hsc privateOwners componentCache
        let prepare roots = do
              selected <- lookupFatIfaceComponents hsc cache privateOwner roots >>= \case
                FatIfaceComponents selection -> pure selection
                _ -> fail "canonical package growth lost its genuine interface selection"
              task <- acquireComponents selected >>= either (fail . show) pure
              runPreparedBodyTask task >>= either (fail . show) pure
            physicalBindings prepared = fmap Map.fromList $ fmap concat $ forM (pmBindings prepared) $ \(binding,_) -> do
              identity <- evaluate binding >>= makeStableName
              pure [(varName binder,identity) | binder <- topBinders binding]
        first <- prepare [privateCallerName]
        firstBindings <- physicalBindings first
        grown <- prepare [privateCallerName,fatIdentityName,privateDiamondName]
        grownBindings <- physicalBindings grown
        assert (all (\(name,identity) -> Map.lookup name grownBindings == Just identity)
          (Map.toList firstBindings))
          "package demand growth re-lowered a completed private component"
        assert (all (\(name,spelling) -> Map.lookup name (pmStableTopSpellings grown) == Just spelling)
          (Map.toList (pmStableTopSpellings first)))
          "package demand growth renamed an already prepared private top"
        shuffled <- prepare [privateDiamondName,fatIdentityName,privateCallerName,privateCallerName]
        shuffledBindings <- physicalBindings shuffled
        assert (grownBindings == shuffledBindings && pmStableTopSpellings grown == pmStableTopSpellings shuffled)
          "root permutation or duplication changed canonical prepared component identities"
        bracket (lookupEnv "TIDEPOOL_DISABLE_BODY_REUSE")
          (maybe (unsetEnv "TIDEPOOL_DISABLE_BODY_REUSE") (setEnv "TIDEPOOL_DISABLE_BODY_REUSE")) $ \_ -> do
            setEnv "TIDEPOOL_DISABLE_BODY_REUSE" "1"
            acquireDisabled <- newPreparedComponentTaskPreparer hsc privateOwners componentCache
            disabledSelection <- lookupFatIfaceComponents hsc cache privateOwner [privateCallerName] >>= \case
              FatIfaceComponents selection -> pure selection
              _ -> fail "body-disable control lost its canonical component"
            disabledTask <- acquireDisabled disabledSelection >>= either (fail . show) pure
            disabledPrepared <- runPreparedBodyTask disabledTask >>= either (fail . show) pure
            disabledBindings <- physicalBindings disabledPrepared
            assert (any (\(name,identity) -> Map.lookup name disabledBindings /= Just identity)
              (Map.toList firstBindings))
              "component body-disable calibration returned completed STG instead of lowering"
            assert (Set.fromList (Map.elems (pmStableTopSpellings disabledPrepared))
                == Set.fromList (Map.elems (pmStableTopSpellings first)))
              "component body-disable calibration changed canonical private spellings"
        selectedCache <- selectPreparedBodyCaches [(componentCache,Set.singleton privateOwner)]
        acquireSelected <- newPreparedComponentTaskPreparer hsc privateOwners selectedCache
        selected <- lookupFatIfaceComponents hsc cache privateOwner [privateCallerName] >>= \case
          FatIfaceComponents selection -> pure selection
          _ -> fail "selected cache fixture lost its exact component"
        selectedTask <- acquireSelected selected >>= either (fail . show) pure
        selectedPrepared <- runPreparedBodyTask selectedTask >>= either (fail . show) pure
        selectedBindings <- physicalBindings selectedPrepared
        assert (selectedBindings == firstBindings)
          "owner-indexed completed cache selection discarded a prepared private unit"
        originalContext <- lookupOwnerInterface privateOwners privateOwner >>= maybe
          (fail "prepared original lacks its declaring context") pure
        independentOwners <- newOwnerInterfaceCache
        acquireIndependent <- newPreparedComponentTaskPreparer hsc independentOwners componentCache
        independentTask <- acquireIndependent selected >>= either (fail . show) pure
        independent <- runPreparedBodyTask independentTask >>= either (fail . show) pure
        independentContext <- lookupOwnerInterface independentOwners privateOwner >>= maybe
          (fail "independently loaded original lacks its declaring context") pure
        assert (not (sameOwnerInterfaceContext originalContext independentContext))
          "independent defining-context issuers aliased the same interface"
        independentBindings <- physicalBindings independent
        assert (any (\(name,identity) -> Map.lookup name independentBindings /= Just identity)
          (Map.toList firstBindings))
          "prepared component cache crossed independently retained declaring contexts"
        independentBodies <- prepareRecoveredBodies hsc independentOwners privateBodies
          privateOwner privateGroups >>= either (fail . show) pure
        oldBodies <- physicalBindings privatePrepared
        newBodies <- physicalBindings independentBodies
        assert (any (\(name,identity) -> Map.lookup name newBodies /= Just identity)
          (Map.toList oldBodies))
          "prepared body cache crossed independently retained declaring contexts"
        retainedAgain <- runPreparedBodyTask =<< (acquireSelected selected >>= either (fail . show) pure)
        retained <- either (fail . show) physicalBindings retainedAgain
        assert (retained == firstBindings)
          "another context's completion displaced the retained original context"
      privateOwnerClone <- liftIO (copyOwnerInterfaceCache privateOwners)
      privateOwnerContext <- liftIO (lookupOwnerInterface privateOwnerClone privateOwner)
      liftIO (assert (maybe False (const True) privateOwnerContext)
        "owner cache copy omitted a completed interface context")
      selectedOwners <- liftIO (mergeOwnerInterfaceCaches
        [(privateOwners,const False),(privateOwnerClone,(== privateOwner))])
      selectedOwnerContext <- liftIO (lookupOwnerInterface selectedOwners privateOwner)
      liftIO (assert (maybe False (const True) selectedOwnerContext)
        "owner selection excluded the earlier map but lost a later selected context")
      excludedOwners <- liftIO (mergeOwnerInterfaceCaches [(privateOwnerClone,const False)])
      excludedOwnerContext <- liftIO (lookupOwnerInterface excludedOwners privateOwner)
      liftIO (assert (maybe True (const False) excludedOwnerContext)
        "owner merge retained an excluded context")
      indexedOwners <- liftIO (selectOwnerInterfaceCaches
        [(privateOwners,Set.empty),(privateOwnerClone,Set.singleton privateOwner)])
      indexedOwnerContext <- liftIO (lookupOwnerInterface indexedOwners privateOwner)
      liftIO (assert (maybe False (const True) indexedOwnerContext)
        "owner-indexed selection lost the requested later context")
      liftIO (evictOwnerInterfaceMatching privateOwners (== privateOwner))
      clonedOwnerContext <- liftIO (lookupOwnerInterface privateOwnerClone privateOwner)
      liftIO (assert (maybe False (const True) clonedOwnerContext)
        "evicting the source owner cache changed its independent copy")
      liftIO $ assert (any ((== "privateHelper") . occNameString . nameOccName . varName)
        (concatMap (topBinders . fst) (pmBindings privatePrepared)))
        "native preparation omitted the defining private helper"
      forward <- liftIO (lookupFatIfaceBodies hsc cache privateOwner [privateCallerName, fatIdentityName])
      reverseRoots <- liftIO (lookupFatIfaceBodies hsc cache privateOwner
        [fatIdentityName, privateCallerName, privateCallerName])
      liftIO $ case (forward, reverseRoots) of
        (FatIfaceFound{}, FatIfaceFound{}) -> assert (foundNames forward == foundNames reverseRoots)
          "root permutation/duplication changed original group membership or defining order"
        _ -> fail "aggregate private-scope roots lost their defining bodies"
      recAResult <- liftIO (lookupFatIfaceExact hsc cache recAName)
      recBResult <- liftIO (lookupFatIfaceExact hsc cache recBName)
      liftIO (assertRecGroup ["recA", "recB"] recAResult)
      liftIO (assertRecGroup ["recA", "recB"] recBResult)
      absentResult <- liftIO (lookupFatIfaceExact hsc cache (missingNameIn fatIdentityName))
      liftIO (assert (isBindingAbsent absentResult)
        "loaded fat interface did not distinguish an absent binding")
      -- Loading the use site at O0 under fat flags can rebuild dependencies.
      -- Restore the deliberately thin artifact and genuine INLINE envelopes
      -- before reading the defining Ids for the absence/read controls.
      liftIO (compileFixture ghc work ["-O1", "-fexpose-all-unfoldings", "-fno-write-if-simplified-core"] thinSource)
      liftIO (compileFixture ghc work
        ["-O1", "-fexpose-all-unfoldings", "-fwrite-if-simplified-core"] missingSource)
      (thinDeclaring, missingDeclaring) <- liftIO $ (,)
        <$> declaringId hsc thinId <*> declaringId hsc missingId
      thinResult <- liftIO (lookupFatIfaceExact hsc cache thinIdentityName)
      liftIO (assert (isNoExtra thinResult)
        "thin interface was not distinguished from an absent binding")
      thinCachedResult <- liftIO (lookupFatIfaceExact hsc cache (missingNameIn thinIdentityName))
      liftIO (assert (isNoExtra thinCachedResult)
        "typed no-extra outcome was not retained in the cache")
      thinRecovery <- liftIO (recoverExactBody hsc cache thinDeclaring)
      liftIO $ case thinRecovery of
        MissingExactBody name NoExtraDeclarations -> assert (name == thinIdentityName)
          "thin recovery refusal changed the exact defining identity"
        _ -> fail "optimizer unfolding bypassed missing original fat Core"
      liftIO (removeFile (work </> "MissingFixture.hi"))
      missingResult <- liftIO (lookupFatIfaceExact hsc cache missingIdentityName)
      liftIO (assertLoadFailure "MissingFixture" missingResult)
      missingCachedResult <- liftIO (lookupFatIfaceExact hsc cache missingIdentityName)
      liftIO (assertLoadFailure "MissingFixture" missingCachedResult)
      missingRecovery <- liftIO (recoverExactBody hsc cache missingDeclaring)
      liftIO $ case missingRecovery of
        BodyInterfaceFailure owner reason -> do
          assert (Just owner == nameModule_maybe missingIdentityName)
            "read failure changed the exact defining owner"
          assert (not (null reason)) "read failure lost its reason"
        _ -> fail "optimizer unfolding bypassed an unreadable defining interface"
  pure ()

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

withTempDirectory :: FilePath -> (FilePath -> IO a) -> IO a
withTempDirectory parent action = do
  (path, handle) <- openTempFile parent "tidepool-fat-iface-exact-"
  hClose handle
  removeFile path
  createDirectoryIfMissing True path
  action path `finally` removePathForcibly path

compileFixture :: FilePath -> FilePath -> [String] -> FilePath -> IO ()
compileFixture ghc work extra source = callProcess ghc
  (["-v0", "-fforce-recomp", "-c", source, "-odir", work, "-hidir", work] ++ extra)

findName :: String -> String -> [Name] -> Name
findName wantedModule wantedOccurrence names = case
    [ name
    | name <- names
    , Just modl <- [nameModule_maybe name]
    , moduleNameString (moduleName modl) == wantedModule
    , occNameString (nameOccName name) == wantedOccurrence
    ] of
  [name] -> name
  found -> error ("expected one " ++ wantedModule ++ "." ++ wantedOccurrence
    ++ ", got " ++ show (length found))

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
    assert (length helpers == 1 && all (not . isExternalName . varName) helpers)
      "private closure did not retain exactly its genuine internal top helper"
    assert (all (not . isLocalId) helpers)
      "private helper control no longer exercises fat-interface GlobalId scope"
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

trim :: String -> String
trim = reverse . dropWhile (== '\n') . reverse
