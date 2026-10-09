{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}

module Main (main, tests) where

import Test.QuickCheck qualified as QC
import CompilerExecutionTest (compilerExecutionTests)
import RecoveryEntryScopeTest (entryScopeTests)
import Tidepool.PreparedStg.Internal (PreparedModule(..))
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Exception (bracket, evaluate, finally, try)
import Control.Monad (forM, forM_, unless, when)
import Control.Monad.IO.Class (liftIO)
import Data.List (stripPrefix)
import Data.Maybe (isJust)
import Data.IORef (newIORef, readIORef, modifyIORef')
import System.Mem.StableName (makeStableName)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC
import GHC.Driver.Env (HscEnv(..), hsc_HPT)
import GHC.Unit.Home.ModInfo (lookupHpt)
import Tidepool.FinalizedModule (FinalizedModule(..))
import GHC.Driver.Main (hscTidy)
import GHC.Driver.Session (updOptLevel)
import GHC.Builtin.Types (intTy, boolTy)
import GHC.Core (Bind(..), Expr(..))
import GHC.Types.Id (mkVanillaGlobal, setIdType, isDataConWorkId_maybe)
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(..))
import GHC.Types.Name (nameOccName, nameModule_maybe)
import GHC.Types.Name (mkSystemName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Occurrence (mkVarOcc)
import GHC.Types.Unique (mkUnique)
import GHC.Types.Var (varName, varUnique)
import GHC.Unit.Types (unitString, GenWithIsBoot(..), mkModule, stringToUnit)
import GHC.Unit.Module.Location (ml_hi_file, ml_dyn_hi_file)
import GHC.Unit.Finder (addModuleToFinder, initFinderCache)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Types.Unique.Set (elementOfUniqSet, nonDetEltsUniqSet)
import GHC.Stg.Syntax qualified as Stg
import System.Directory (getCurrentDirectory, removeFile)
import System.Environment (setEnv)
import System.IO (openTempFile, stderr, hClose, hFlush, hPutStr, hSeek, hGetContents, SeekMode(..))
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import System.Exit (ExitCode(..))
import System.FilePath ((</>))
import System.Process (proc, readCreateProcessWithExitCode)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), preparedTopIdentities, preparedTopIdentityBindings
  , prepareProjection, prepareProjectionWithReachability, prepareComponentProjectionWithReachability, projectSelected
  , projectPreparedTarget, preparedModuleReachFacts, preparedSeedUniques
  , admitReachFacts, emptyPreparedReachability, reachedUniques, admittedTops
  , PreparedReachUpdate(..), updatePreparedReachability
  , preparedTargetReferences, preparedRootIdentity, resolveTextPackageUnit )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), Group(..), HeapBinding(..)
  , GlobalDecl(..), HeapRhs(..), SymbolIdentity(..), TargetDescriptor(..)
  , TopBinding(..), WireProgram(..) )
import Tidepool.FatIface
  ( newFatIfaceCache, newOwnerInterfaceCache, lookupFatIfaceComponents
  , FatIfaceComponentLookup(..), fatSelectionComponents
  , readExactInterface, lookupFatIfaceExact, FatIfaceLookup(..), FatIfaceMissing(..)
  , OwnerInterfaceContext, copyOwnerInterfaceCache, lookupOwnerInterface, cacheOwnerInterface
  , selectOwnerInterfaceCaches, evictOwnerInterfaceMatching, sameOwnerInterfaceContext )
import Tidepool.PreparedRecovery
  ( RecoveryFailure(..), RecoveredClosure(..), insertGroup
  , RecoveryPublicationFailure(..), requirePreparedRecoveryPublication
  , recoverPreparedClosure, newPreparedRecovery, newPreparedRecoveryWithPackageRoots, newPreparedRecoveryWithDemand
  , preparedRecoveryClosure, growPreparedRecovery )
import Tidepool.CompilerExecution (withCompilerExecutor, compilerExecutionGrant)
import Tidepool.OriginalProductRoots (requiredOriginalPackageGlobalsWithRetained)
import Tidepool.CertifiedProducts (resolvePackageGlobal)
import Tidepool.PreparedStg
  ( PreparedCoverage(..), pmModule, pmCoverage, pmBindings, pmSiteRejections, RecoveredModuleFailure(..)
  , newPreparedBodyCache, prepareModule, prepareRecoveredBodies, newPreparedComponentTaskPreparer, runPreparedBodyTask
  , preparedExpectedEntry )
import Tidepool.PreparedSites (SiteRejection(..))

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "test-prepared-stg"
  [ compilerExecutionTests
  , entryScopeTests
  , testCase "component histories agree with complete reachability recomputation" reachHistoryProperty
  , testCase "overlapping structural groups retain every sibling" assertOverlapMerge
  , testCase "subset lookup preserves authoritative full group" assertSubsetPreservesFullGroup
  , testCase "compiled original recovery and reachability closure" scenario
  ]

-- Generated replacement histories exercise the production reach update owner.
-- The independent oracle always scans the current adjacency from all roots.
data ReachOperation = AdmitNode Int [Int] | ReplaceArena [(Int,[Int])] | AddRoot Int
  deriving Show

reachHistoryProperty :: IO ()
reachHistoryProperty = do
  result <- QC.quickCheckWithResult QC.stdArgs { QC.maxSuccess=160, QC.maxSize=40 }
    (QC.checkCoverage $ QC.forAllShrink history (QC.shrinkList shrinkOperation) $ \operations ->
      QC.cover 20 (any isReplacement operations) "site replacement" $
      QC.cover 20 (any isAdmission operations) "pure admission" $
      QC.cover 20 (any isRoot operations) "root growth" $
      QC.cover 5 (any selfCycle operations) "cycles" $
      QC.counterexample (show operations) (runHistory False operations))
  unless (QC.isSuccess result) (fail "component reachability history differs from recomputation")
  -- Mutation calibration: retaining a removed site's edge keeps an unrelated
  -- top reachable. The production replacement must remove that old demand.
  let removed = [AddRoot 0,ReplaceArena [(0,[1]),(1,[])],ReplaceArena [(0,[]),(1,[])]]
  unless (runHistory False removed && not (runHistory True removed))
    (fail "site-edge union mutation was not detected")
  where
    history = QC.listOf $ QC.frequency
      [(4,AdmitNode <$> vertex <*> QC.listOf vertex)
      ,(2,ReplaceArena <$> QC.listOf ((,) <$> vertex <*> QC.listOf vertex))
      ,(2,AddRoot <$> vertex)]
    vertex = QC.chooseInt (0,6)
    isReplacement ReplaceArena{} = True
    isReplacement _ = False
    isAdmission AdmitNode{} = True
    isAdmission _ = False
    isRoot AddRoot{} = True
    isRoot _ = False
    selfCycle (AdmitNode node edges) = node `elem` edges
    selfCycle (ReplaceArena rows) = any (\(node,edges) -> node `elem` edges) rows
    selfCycle _ = False
    shrinkOperation operation = case operation of
      AdmitNode node edges -> [AdmitNode smaller remaining | (smaller,remaining) <- QC.shrink (node,edges)]
      ReplaceArena rows -> map ReplaceArena (QC.shrink rows)
      AddRoot root -> map AddRoot (QC.shrink root)
    identifiers = [mkVanillaGlobal (mkSystemName (mkUnique 'r' index) (mkVarOcc ("history" ++ show index))) intTy
      | index <- [0..6]]
    identifier index = identifiers !! index
    facts rows = [[(identifier node,map (varUnique . identifier) edges) | (node,edges) <- Map.toAscList rows]]
    keySet uniques = Set.fromList [index | index <- [0..6]
      , varUnique (identifier index) `elementOfUniqSet` uniques]
    runHistory mutate = walk Map.empty [] emptyPreparedReachability
      where
        walk _ _ _ [] = True
        walk rows roots carried (operation:remaining) =
          let (nextRows,nextRoots,update) = case operation of
                AdmitNode node edges -> case Map.lookup node rows of
                  Just _ -> (rows,roots,AdmitPreparedFacts [])
                  Nothing -> (Map.insert node edges rows,roots,AdmitPreparedFacts (facts (Map.singleton node edges)))
                ReplaceArena incoming ->
                  let replaced = Map.fromList incoming
                  in (replaced,roots,if mutate then AdmitPreparedFacts (facts replaced) else ReplacePreparedFacts (facts replaced))
                AddRoot root -> (rows,root:roots,AdmitPreparedFacts [])
              actual = updatePreparedReachability (map (varUnique . identifier) nextRoots) update carried
              expected = fullReach nextRows nextRoots
          in keySet (reachedUniques actual) == expected
              && keySet (admittedTops actual) == Map.keysSet nextRows
              && walk nextRows nextRoots actual remaining
    fullReach rows roots = grow Set.empty roots
      where
        grow seen [] = seen
        grow seen (node:pending)
          | node `Set.member` seen = grow seen pending
          | otherwise = grow (Set.insert node seen) (Map.findWithDefault [] node rows ++ pending)

-- The two values are issued by real interface reads/typechecks. Histories
-- exercise only custody operations, never fabricate declaring authority.
data ContextOperation
  = InstallContext Int Int
  | CopyContext Int Int
  | EvictContext Int
  | SelectContexts Int Int Int Bool Bool
  deriving Show

verifyContextHistories :: Module -> OwnerInterfaceContext -> OwnerInterfaceContext -> IO ()
verifyContextHistories owner first second = do
  let generator = QC.listOf $ QC.oneof
        [ InstallContext <$> slot <*> QC.chooseInt (0,1)
        , CopyContext <$> slot <*> slot
        , EvictContext <$> slot
        , SelectContexts <$> slot <*> slot <*> slot <*> QC.arbitrary <*> QC.arbitrary ]
      slot = QC.chooseInt (0,2)
      contexts = [first,second]
      history mutate operations = do
        initial <- mapM (const newOwnerInterfaceCache) [0..2 :: Int]
        let observe caches expected = do
              actual <- mapM (\cache -> lookupOwnerInterface cache owner) caches
              pure $ and [case (value,model) of
                (Nothing,Nothing) -> True
                (Just retained,Just identity) -> sameOwnerInterfaceContext retained (contexts !! identity)
                _ -> False | (value,model) <- zip actual expected]
            replace index value rows = take index rows ++ [value] ++ drop (index+1) rows
            walk _ _ [] = pure True
            walk caches expected (operation:rest) = do
              (next,model) <- case operation of
                InstallContext target identity -> do
                  cacheOwnerInterface (caches !! target) owner (contexts !! (if mutate then 0 else identity))
                  pure (caches,replace target (Just identity) expected)
                CopyContext target source -> do
                  copied <- copyOwnerInterfaceCache (caches !! source)
                  pure (replace target copied caches,replace target (expected !! source) expected)
                EvictContext target -> do
                  evictOwnerInterfaceMatching (caches !! target) (== owner)
                  pure (caches,replace target Nothing expected)
                SelectContexts target left right keepLeft keepRight -> do
                  selected <- selectOwnerInterfaceCaches
                    [(caches !! left, if keepLeft then Set.singleton owner else Set.empty)
                    ,(caches !! right,if keepRight then Set.singleton owner else Set.empty)]
                  let choose = case if keepLeft then expected !! left else Nothing of
                        Just value -> Just value
                        Nothing -> if keepRight then expected !! right else Nothing
                  pure (replace target selected caches,replace target choose expected)
              correct <- observe next model
              if correct then walk next model rest else pure False
        walk initial (replicate 3 Nothing) operations
  let calibrationSteps = [InstallContext 0 0,CopyContext 1 0,InstallContext 0 1
        ,SelectContexts 2 1 0 True True,EvictContext 1]
  calibration <- history False calibrationSteps
  mutated <- history True calibrationSteps
  assert (calibration && not mutated)
    "declaring-context issuer-alias mutation escaped custody recomputation"
  -- Matching interface bytes alone is a deliberately invalid cache identity.
  assert (not (sameOwnerInterfaceContext first second))
    "context identity mutation escaped the independently loaded-owner control"
  result <- QC.quickCheckWithResult QC.stdArgs { QC.maxSuccess=120, QC.maxSize=40 }
    (QC.checkCoverage $ QC.forAllShrink generator (QC.shrinkList (const [])) $ \operations ->
      QC.cover 20 (any (\case CopyContext{} -> True; _ -> False) operations) "copy" $
      QC.cover 20 (any (\case SelectContexts{} -> True; _ -> False) operations) "selected closure" $
      QC.cover 20 (any (\case EvictContext{} -> True; _ -> False) operations) "eviction" $
      QC.cover 20 (any (\case InstallContext _ 1 -> True; _ -> False) operations) "changed issuer" $
      QC.counterexample (show operations) (QC.ioProperty (history False operations)))
  unless (QC.isSuccess result) (fail "retained declaring-context histories diverged")

scenario :: IO ()
scenario = do
  root <- getCurrentDirectory
  let source = root </> "test-prepared-stg" </> "RecoveryCaller.hs"
      hiddenSource = root </> "test-prepared-stg" </> "RecoveryHiddenText.hs"
      includes = [root </> "test-prepared-stg"]
  libdir <- trim <$> readProcessGhc ["--print-libdir"]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (updOptLevel 0 flags)
      { importPaths = includes
      , backend = noBackend
      , ghcLink = NoLink
      }
    target <- guessTarget source Nothing Nothing
    hiddenTarget <- guessTarget hiddenSource Nothing Nothing
    setTargets [target, hiddenTarget]
    _ <- load LoadAllTargets
    hsc <- getSession
    selectedText <- liftIO (resolveTextPackageUnit hsc)
    home <- prepareNamed hsc "RecoveryHome"
    caller <- prepareNamed hsc "RecoveryCaller"
    hidden <- prepareNamed hsc "RecoveryHiddenText"
    let modules = [home, caller]
        entry = callerEntry modules
        context = ProjectionContext
          { projectionProfile = Text.pack "w5-b3-prepared-recovery"
          , projectionToolchain = Text.pack "ghc-9.12.2"
          , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
              (Text.pack "sysv64") []
          , projectionRetainedGenerations = mempty
          , projectionCurrentOriginals = mempty
          , projectionEntry = entry
          , projectionAuxiliaryRoots = []
          , projectionFormattingAuthority = Nothing
          , projectionTimeAuthority = Nothing
          , projectionJsonAuthority = Nothing
          , projectionTextUnit = selectedText
          }
    closure <- liftIO $ do
      cache <- newFatIfaceCache
      ownerCache <- newOwnerInterfaceCache
      bodyCache <- newPreparedBodyCache
      recover <- newPreparedRecovery hsc cache ownerCache bodyCache context modules
      first <- recover entry
      let fstUnits = [prepared | prepared <- closureModules first, recoveredFst prepared]
      (fstOwner, fstName) <- case fstUnits of
        [prepared] -> case [varName binder | binder <- topBindersOfModule prepared
              , occNameString (nameOccName (varName binder)) == "fst"] of
          [name] -> pure (pmModule prepared,name)
          _ -> fail "retained context history lacks an exact fst binder"
        _ -> fail "retained context history lacks one fst unit"
      firstContext <- lookupOwnerInterface ownerCache fstOwner >>= maybe
        (fail "recovered fst lacks its retained context") pure
      independentOwners <- newOwnerInterfaceCache
      groups <- lookupFatIfaceExact hsc cache fstName >>= \case
        FatIfaceFound bodies -> pure bodies
        _ -> fail "context history lost the exact original fst body"
      _ <- prepareRecoveredBodies hsc independentOwners bodyCache fstOwner groups >>= either (fail . show) pure
      secondContext <- lookupOwnerInterface independentOwners fstOwner >>= maybe
        (fail "independent fst load lacks its retained context") pure
      assert (not (sameOwnerInterfaceContext firstContext secondContext))
        "independent retained dependency contexts share an issuer"
      verifyContextHistories fstOwner firstContext secondContext
      otherEntry <- case [identity | identity <- either (error . show) id (preparedTopIdentities [home])
                                  , symbolOccurrence identity == Text.pack "homeOther"] of
        [identity] -> pure identity
        found -> fail ("expected homeOther entry, got " ++ show found)
      other <- recover otherEntry
      assertProjectionEquivalent (context { projectionEntry = otherEntry }) other
      assert (not (any recoveredFst (closureModules other)))
        "second target inherited the first target's fst dependency"
      repeated <- recover entry
      assertProjectionEquivalent context repeated
      assert (any recoveredFst (closureModules repeated))
        "returning to the first target lost its defining-module recovery"
      assert (closureFactCacheHits repeated > 0)
        "returning to the first target did not reuse recovered-body facts"
      assert (closureFactCacheHits other == 0)
        "a target without the recovered dependency observed stale recovery facts"
      pure first
    liftIO $ assert (any recoveredFst (closureModules closure))
      "closure did not retain the newly prepared defining module for fst"
    liftIO $ assertProjectionEquivalent context closure
    liftIO $ verifyRecoveryPublication hsc context modules closure
    liftIO $ assertReachExpansion context closure
    liftIO $ assertRejectionBoundary context closure
    liftIO $ assert (all (not . namedResidual) (closureFailures closure))
      ("nullary constructor remained a recovery residual: " ++ show (closureFailures closure))
    liftIO $ assert (all (\original -> originalModuleRetained original (closureModules closure)) modules)
      "recovery dropped an original prepared module"
    liftIO $ assertNullaryRecoveryProjection context closure
    liftIO $ incompleteSubsetContract home context
    hiddenClosure <- liftIO $ hidden_defining_module hsc context hidden
    liftIO $ growing_original_packages hsc context home hidden hiddenClosure closure
    queueOwner <- findModule (mkModuleName "Data.FTCQueue") Nothing
    liftIO $ constructor_component_recovery hsc context queueOwner root source
    let typeableOwner = mkModule (stringToUnit "ghc-internal")
          (mkModuleName "GHC.Internal.Data.Typeable.Internal")
    liftIO $ evaluated_constructor_component hsc context typeableOwner
    liftIO $ retainedProjectionBoundary hsc context modules
    liftIO $ putStrLn "prepared recovery closure: ok"
  where
    evaluated_constructor_component hsc context owner = do
      let root = SymbolIdentity (Text.pack (unitString (moduleUnit owner)))
            (Text.pack (moduleNameString (moduleName owner))) "value" "mkTrCon11" Nothing
          selectedContext = context { projectionEntry = root }
      (identifier,_) <- resolvePackageGlobal hsc root >>= either fail pure
      cache <- newFatIfaceCache
      owners <- newOwnerInterfaceCache
      bodies <- newPreparedBodyCache
      selection <- lookupFatIfaceComponents hsc cache owner [varName identifier] >>= \case
        FatIfaceComponents selected -> pure selected
        _ -> fail "Typeable constructor entry has no genuine original component"
      acquire <- newPreparedComponentTaskPreparer hsc owners bodies
      task <- acquire selection >>= either (fail . show) pure
      prepared <- runPreparedBodyTask task >>= either (fail . show) pure
      let canonical = [original | (binding,_) <- pmBindings prepared, binder <- topBinders binding
            , varName binder == varName identifier, Just original <- [preparedExpectedEntry prepared binder]]
          canonicalConstructor = case canonical of
            [original] -> case importedIdLFInfo original of
              LFCon{} -> True
              _ -> False
            _ -> False
      assert canonicalConstructor
        "Typeable regression lacks a canonical evaluated constructor entry"
      program <- either (fail . show) pure (projectPreparedTarget selectedContext [prepared])
      let constructors = [identity | group <- programBindings program
            , TopBinding identity (HeapBinding _ Constructor{}) <- case group of
                NonRecursive top -> [top]; Recursive tops -> tops]
      assert (root `elem` constructors)
        "canonical evaluated Typeable entry became a thunk during subset preparation"
      where
        topBinders (Stg.StgTopLifted (Stg.StgNonRec binder _)) = [binder]
        topBinders (Stg.StgTopLifted (Stg.StgRec pairs)) = map fst pairs
        topBinders (Stg.StgTopStringLit binder _) = [binder]

    constructor_component_recovery hsc context owner root source = do
      let symbol occurrence = SymbolIdentity
            (Text.pack (unitString (moduleUnit owner))) (Text.pack (moduleNameString (moduleName owner)))
            "value" occurrence Nothing
          workerRoot = symbol "$WNode"
          viewRoot = symbol "tviewl_go"
          packageContext = context
            { projectionEntry = viewRoot, projectionAuxiliaryRoots = [workerRoot] }
      identifiers <- forM [viewRoot, workerRoot] $ \identity -> do
        (identifier, _) <- resolvePackageGlobal hsc identity >>= either fail pure
        assert (preparedRootIdentity identifier == identity) "queue root differs from canonical package Id"
        pure identifier
      cache <- newFatIfaceCache
      owners <- newOwnerInterfaceCache
      bodies <- newPreparedBodyCache
      acquire <- newPreparedComponentTaskPreparer hsc owners bodies
      let prepare roots = do
            selection <- lookupFatIfaceComponents hsc cache owner (map varName roots) >>= \case
              FatIfaceComponents selected -> pure selected
              _ -> fail "real FTCQueue component selection failed"
            task <- acquire selection >>= either (fail . show) pure
            prepared <- runPreparedBodyTask task >>= either (fail . show) pure
            pure (selection, prepared)
          bindings prepared = fmap Map.fromList $ fmap concat $ forM (pmBindings prepared) $ \(binding, _) -> do
            physical <- evaluate binding >>= makeStableName
            pure [(varName binder, physical) | binder <- topBinders binding]
          workerNames prepared = [varName binder | (binding, _) <- pmBindings prepared
            , binder <- topBinders binding, isJust (isDataConWorkId_maybe binder)]
      (_, first) <- prepare (take 1 identifiers)
      firstBindings <- bindings first
      (selection, grown) <- prepare identifiers
      assert (length (fatSelectionComponents selection) > 1)
        "queue regression did not select independent components sharing a datatype"
      let names = map varName (topBindersOfModule grown)
          workers = workerNames grown
      assert (length names == Set.size (Set.fromList names)) "queue assembly duplicated a top"
      assert (not (null workers) && length workers == Set.size (Set.fromList workers))
        "queue assembly lost or duplicated defining constructor workers"
      grownBindings <- bindings grown
      assert (all (\(name, physical) -> Map.lookup name grownBindings == Just physical)
        (Map.toList firstBindings)) "queue growth re-lowered completed component or constructor workers"
      (_, repeated) <- prepare (reverse identifiers ++ identifiers)
      repeatedBindings <- bindings repeated
      assert (grownBindings == repeatedBindings) "queue root permutation re-lowered completed units"
      projected <- either (fail . show) pure (projectPreparedTarget packageContext [grown])
      repeatedProgram <- either (fail . show) pure (projectPreparedTarget packageContext [repeated])
      assert (projected == repeatedProgram) "queue root permutation changed executable identities"
      let defined = Set.fromList [identity | group <- programBindings projected
            , TopBinding identity _ <- case group of NonRecursive top -> [top]; Recursive tops -> tops]
      assert (viewRoot `Set.member` defined && workerRoot `Set.member` defined)
        "projected queue package closure lacks demanded executable definitions"
      -- The same authored producer builds and deconstructs a Node and Leaf;
      -- the central resident smoke supplies native execution of this package.
      output <- readProcessGhc ["-v0", "-i" ++ (root </> "test-prepared-stg"), source, "-e", "queueProbe"]
      assert (trim output == "42") "GHC queue constructor oracle changed"

    projectionResult projection = fmap fst (projection >>= projectSelected)

    assertProjectionEquivalent context closure = do
      let modules = closureModules closure
          old = projectionResult (prepareProjection context modules)
          carried = projectionResult
            (prepareProjectionWithReachability context modules (closureReachability closure))
          component = projectionResult
            (prepareComponentProjectionWithReachability context (closureHomeModules closure)
              (closureComponentSelections closure) (closureReachability closure))
      assert (old == component) "component-backed projection changed complete recomputation"
      assert (old == carried)
        ("carried reachability changed projection for " ++ show (projectionEntry context)
          ++ ": " ++ show (fmap (length . programBindings) old)
          ++ " vs " ++ show (fmap (length . programBindings) carried))

    assertReachExpansion context closure = do
      let modules = closureModules closure
          home = take 2 modules
          recovered = drop 2 modules
          seeds = nonDetEltsUniqSet (preparedSeedUniques context home)
          initial = admitReachFacts seeds (map (preparedModuleReachFacts context) home)
            emptyPreparedReachability
          recoveredFacts = map (preparedModuleReachFacts context) recovered
          partial = admitReachFacts seeds
            (map (map (\(binder, _) -> (binder, []))) recoveredFacts) initial
          expanded = admitReachFacts seeds recoveredFacts partial
      assert (all (`elementOfUniqSet` reachedUniques expanded) seeds)
        "replacement lost seeded tops"
      assert (projectionResult
        (prepareProjectionWithReachability context modules expanded)
          == projectionResult (prepareProjection context modules))
        "incrementally admitted recovered modules changed selection"

    assertRejectionBoundary context closure = do
      let modules = closureModules closure
      case modules of
        home : rest -> case [binder | binder <- topBindersOfModule home
            , varUnique binder `elementOfUniqSet` reachedUniques (closureReachability closure)] of
          selectedBinder : _ -> do
            let rejected = home { preparedSiteRejections =
                  SiteRejection selectedBinder "injected site" : pmSiteRejections home }
                withRejection = rejected : rest
                old = prepareProjection context withRejection
                carried = prepareProjectionWithReachability context withRejection
                  (closureReachability closure)
            assert (fmap (const ()) old == fmap (const ()) carried)
              "carried reachability changed typed site rejection"
            case carried of
              Left (RejectedTypedSite message) | message == "injected site" -> pure ()
              _ -> ioError (userError "reachable injected site was not rejected")
          [] -> ioError (userError "no reached home binder for rejection check")
        [] -> ioError (userError "recovery returned no modules")

    retainedProjectionBoundary hsc context modules = do
      let home = case modules of
            first : _ -> first
            [] -> error "retained projection fixture has no modules"
          identity occurrence = case [symbol | symbol <- either (error . show) id
              (preparedTopIdentities [home]), symbolOccurrence symbol == occurrence] of
            [symbol] -> symbol
            found -> error ("expected " ++ Text.unpack occurrence ++ " identity, got " ++ show found)
          retainedContext = context
            { projectionEntry = identity "homeOther"
            , projectionRetainedGenerations =
                Map.singleton (identity "homeValue") 11
            }
      cache <- newFatIfaceCache
      ownerCache <- newOwnerInterfaceCache
      bodyCache <- newPreparedBodyCache
      retained <- recoverPreparedClosure hsc cache ownerCache bodyCache retainedContext modules
      assertProjectionEquivalent retainedContext retained
      case projectionResult
        (prepareProjectionWithReachability retainedContext (closureModules retained)
          (closureReachability retained)) of
        Right program -> assert
          (any (\global -> globalIdentity global == identity "homeValue"
              && globalRequiredGeneration global == Just 11) (programGlobals program))
          "retained top did not become a generation-bound global"
        Left failure -> ioError (userError ("retained projection failed: " ++ show failure))
      case closureModules retained of
        retainedHome : rest -> case [binder | binder <- topBindersOfModule retainedHome
            , occNameString (nameOccName (varName binder)) == "homeValue"] of
          retainedBinder : _ -> do
            let markedHome = retainedHome
                  { preparedSiteRejections = SiteRejection retainedBinder "skipped site"
                      : pmSiteRejections retainedHome }
                markedModules = markedHome : rest
            assert (projectionResult
              (prepareProjectionWithReachability retainedContext markedModules
                (closureReachability retained))
                == projectionResult (prepareProjection retainedContext markedModules))
              "retained top's skipped site changed projection"
            case prepareProjectionWithReachability retainedContext markedModules
              (closureReachability retained) of
              Right _ -> pure ()
              Left failure -> ioError (userError
                ("retained top's skipped site was rejected: " ++ show failure))
          [] -> ioError (userError "retained fixture has no homeValue binder")
        [] -> ioError (userError "retained recovery returned no modules")

    topBindersOfModule prepared = concatMap (topBinders . fst) (pmBindings prepared)


    prepareNamed hsc name = do
      summary <- getModSummary (mkModuleName name)
      parsed <- parseModule summary
      typed <- typecheckModule parsed
      desugared <- desugarModule typed
      (guts, _) <- liftIO $ hscTidy hsc (coreModule desugared)
      home <- maybe (fail "loaded module has no finalized home interface") pure
        (lookupHpt (hsc_HPT hsc) (ms_mod_name summary))
      liftIO $ prepareModule hsc (ms_location summary) mempty
        (FinalizedModule home guts)

    trim = reverse . dropWhile (== '\n') . reverse

    readProcessGhc args = do
      (code, out, err) <- readCreateProcessWithExitCode (proc "ghc" args) ""
      case code of
        ExitSuccess -> pure out
        _ -> ioError (userError ("ghc failed: " ++ err))

    callerEntry modules = case
      [ identity
      | identity <- either (error . show) id (preparedTopIdentities modules)
      , symbolOccurrence identity == Text.pack "caller"
      ] of
      [identity] -> identity
      found -> error ("expected one caller entry, got " ++ show found)

    recoveredFst prepared =
      moduleNameString (moduleName (pmModule prepared)) /= "RecoveryHome"
      && moduleNameString (moduleName (pmModule prepared)) /= "RecoveryCaller"
      && any topIsFst (pmBindings prepared)

    hidden_defining_module hsc context hidden = do
      hiddenEntry <- case
        [ identity
        | identity <- either (error . show) id (preparedTopIdentities [hidden])
        , symbolOccurrence identity == Text.pack "hiddenText"
        ] of
        [identity] -> pure identity
        found -> ioError (userError
          ("expected one hiddenText entry, got " ++ show found))
      let hiddenContext = context { projectionEntry = hiddenEntry }
      cache <- newFatIfaceCache
      ownerCache <- newOwnerInterfaceCache
      bodyCache <- newPreparedBodyCache
      closure <- recoverPreparedClosure hsc cache ownerCache bodyCache hiddenContext [hidden]
      let isTextShow owner = moduleNameString (moduleName owner) == "Data.Text.Show"
          preparedTextShow = [ prepared
            | prepared <- closureModules closure, isTextShow (pmModule prepared) ]
          finderResiduals = [ failure
            | failure@(DefiningPreparationFailure (RecoveredModuleFinderFailure owner _))
                <- closureFailures closure
            , isTextShow owner ]
      assert (any (not . null . pmBindings) preparedTextShow)
        ("hidden defining Data.Text.Show body was not prepared: "
          ++ show (closureFailures closure))
      assert (null finderResiduals)
        ("hidden defining module retained a finder residual: "
          ++ show finderResiduals)

      pure closure

    growing_original_packages hsc context home hidden hiddenClosure callerClosure = do
      let identity prepared occurrence = case
            [symbol | symbol <- either (error . show) id (preparedTopIdentities [prepared])
            , symbolOccurrence symbol == occurrence] of
              [symbol] -> symbol
              found -> error ("unexpected original-package fixture identity: " ++ show found)
          hiddenContext = context { projectionEntry = identity hidden "hiddenText" }
          textRoots = [binder | binder <- preparedTargetReferences hiddenContext [hidden]
            , occNameString (nameOccName (varName binder)) == "$fShowText"]
      textRoot <- case textRoots of
        [binder] -> pure binder
        found -> fail ("expected one Text Show dictionary, got " ++ show (length found))
      fstRoot <- case [binder | prepared <- closureModules callerClosure
          , binder <- topBindersOfModule prepared
          , occNameString (nameOccName (varName binder)) == "fst"] of
        [binder] -> pure binder
        found -> fail ("expected one recovered fst, got " ++ show (length found))
      let showUnits = [prepared | prepared <- closureModules hiddenClosure
            , moduleNameString (moduleName (pmModule prepared)) == "GHC.Internal.Show"]
      showOwner <- case showUnits of
        prepared : _ -> pure prepared
        [] -> fail "recovered Show owner has no component units"
      let sourceRoot = identity home "homeValue"
          entry = identity home "homeOther"
          subset = home { preparedCoverage = ExactBodySubset
            , preparedBindings = filter (any ((== "homeOther") . occNameString . nameOccName . varName)
                . topBinders . fst) (pmBindings home) }
          fixtureContext = context
            { projectionEntry = entry
            , projectionCurrentOriginals = Map.union
                (Map.filter (== sourceRoot) (preparedTopIdentityBindings [home]))
                (preparedTopIdentityBindings showUnits) }
          owner prepared = (unitString (moduleUnit (pmModule prepared)),
            moduleNameString (moduleName (pmModule prepared)))
          (showUnit, showName) = owner showOwner
          (homeUnit, homeName) = owner home
          showBinders = either (error . show) id (preparedTopIdentities showUnits)
          -- Original outlines deliberately retain one package dependency per
          -- group. The second original owner becomes demanded only after the
          -- Text dictionary has been recovered and projected.
          originals =
            [(homeUnit, homeName, [(0, [sourceRoot], [(preparedRootIdentity textRoot, True)])]),
             (showUnit, showName, [(0, showBinders, [(preparedRootIdentity fstRoot, True)])])]
          project roots closed = either (fail . show) (pure . fst) $
            prepareComponentProjectionWithReachability
              (fixtureContext { projectionAuxiliaryRoots = roots })
              (closureHomeModules closed) (closureComponentSelections closed)
              (closureReachability closed) >>= projectSelected
          required program = either fail pure $
            requiredOriginalPackageGlobalsWithRetained [] [] originals Set.empty (programGlobals program)
          resolve roots = forM roots $ \symbol -> do
            (binder, _) <- resolvePackageGlobal hsc symbol >>= either fail pure
            assert (preparedRootIdentity binder == symbol) "fixture resolved a noncanonical package root"
            pure binder
      cache <- newFatIfaceCache
      ownerCache <- newOwnerInterfaceCache
      bodyCache <- newPreparedBodyCache
      setEnv "TIDEPOOL_TIMING" "1"
      factory <- newPreparedRecoveryWithPackageRoots hsc cache ownerCache bodyCache fixtureContext [subset] []
      (initial, initialCount) <- measurePreparations (factory entry)
      initialProgram <- project [] (preparedRecoveryClosure initial)
      firstRoots <- required initialProgram
      assert (firstRoots == [preparedRootIdentity textRoot]) "initial original group did not demand Text"
      (first, firstCount) <- measurePreparations (resolve firstRoots >>= growPreparedRecovery initial)
      firstProgram <- project firstRoots (preparedRecoveryClosure first)
      secondDemand <- required firstProgram
      let allRoots = Set.toAscList (Set.fromList (firstRoots ++ secondDemand))
      assert (preparedRootIdentity fstRoot `elem` secondDemand && allRoots /= firstRoots)
        "recovered Text code did not expose the second original group"
      (final, finalCount) <- measurePreparations (resolve allRoots >>= growPreparedRecovery first)
      finalProgram <- project allRoots (preparedRecoveryClosure final)
      finalDemand <- required finalProgram
      assert (all (`elem` allRoots) finalDemand) "two-round fixture did not close package demand"
      forM_ [(cold, jobs) | cold <- [False, True], jobs <- [1,2]] $ \(cold, jobCount) -> do
        (pumpCache, pumpOwners, pumpBodies) <- if cold
          then (,,) <$> newFatIfaceCache <*> newOwnerInterfaceCache <*> newPreparedBodyCache
          else pure (cache, ownerCache, bodyCache)
        grant <- either fail pure (compilerExecutionGrant jobCount)
        waves <- newIORef []
        withCompilerExecutor grant $ \executor -> do
          let demand _ packageRoots closed = do
                let currentRoots = Set.toAscList (Set.fromList (map preparedRootIdentity packageRoots))
                modifyIORef' waves (++ [Set.fromList currentRoots])
                current <- project currentRoots closed
                next <- required current
                resolve (Set.toAscList (Set.fromList next `Set.difference` Set.fromList currentRoots))
          pumping <- newPreparedRecoveryWithDemand (Just executor) demand hsc pumpCache pumpOwners pumpBodies
            fixtureContext [subset] []
          (pumped, counters) <- measureRecoveryCounts (pumping entry)
          let count name = Map.findWithDefault 0 ("prepared_recover_" ++ name) counters
          when cold $ do
            assert (count "unit_preparations" > 0
                && count "units_started" == count "unit_preparations"
                && count "units_completed" == count "units_started"
                && count "units_failed" == 0
                && count "component_new_groups" > 0
                && count "unit_fact_admissions" > 0
                && count "package_root_additions" == fromIntegral (length allRoots))
              ("cold completion pump did not execute and settle exact component work: " ++ show counters)
          putStrLn ("package completion pump: cold=" ++ show cold ++ ", jobs=" ++ show jobCount
            ++ ", counters=" ++ show (Map.toAscList counters))
          observed <- readIORef waves
          assert (observed == map Set.fromList [[],firstRoots,allRoots])
            ("completion pump changed projected package demand waves: " ++ show observed)
          pumpedProgram <- project allRoots (preparedRecoveryClosure pumped)
          assert (pumpedProgram == finalProgram
              && closureFailures (preparedRecoveryClosure pumped) == closureFailures (preparedRecoveryClosure final))
            "component completion pump disagreed with independent outer-loop recomputation"
      -- Compare with the old outer loop at identical roots, graph and authority.
      oldResults <- forM [[], firstRoots, allRoots] $ \roots -> do
        binders <- resolve roots
        restart <- newPreparedRecoveryWithPackageRoots hsc cache ownerCache bodyCache
          fixtureContext [subset] binders
        measurePreparations (fmap preparedRecoveryClosure (restart entry))
      let oldClosures = map fst oldResults
      oldPrograms <- sequence (zipWith project [[], firstRoots, allRoots] oldClosures)
      assert (oldPrograms == [initialProgram, firstProgram, finalProgram])
        "growing package recovery changed canonical projected programs"
      let newClosures = map preparedRecoveryClosure [initial, first, final]
          oldCount = sum (map snd oldResults)
          newCount = initialCount + firstCount + finalCount
      assert (newCount < oldCount) "continuation did not remove defining-module preparation calls"
      assert (map closureFailures oldClosures == map closureFailures newClosures)
        "continuation changed typed failure witnesses"
      (repeated, repeatedCount) <- measurePreparations (resolve allRoots >>= growPreparedRecovery final)
      assert (repeatedCount == 0)
        "unchanged roots repeated defining-module preparation"
      repeatedProgram <- project allRoots (preparedRecoveryClosure repeated)
      assert (repeatedProgram == finalProgram) "unchanged roots changed canonical output"
      isolated <- factory entry
      isolatedProgram <- project [] (preparedRecoveryClosure isolated)
      assert (isolatedProgram == initialProgram)
        "fresh target inherited grown package roots or body sets"
      let retainedContext = fixtureContext
            { projectionRetainedGenerations = Map.singleton sourceRoot 11 }
      retainedFactory <- newPreparedRecoveryWithPackageRoots hsc cache ownerCache bodyCache
        retainedContext [subset] []
      retained <- retainedFactory entry >>= (\state -> growPreparedRecovery state [fstRoot])
      let retainedClosure = preparedRecoveryClosure retained
      retainedProgram <- either (fail . show) (pure . fst) $
        prepareProjectionWithReachability
          (retainedContext { projectionAuxiliaryRoots = [preparedRootIdentity fstRoot] })
          (closureModules retainedClosure) (closureReachability retainedClosure) >>= projectSelected
      retainedDemand <- required retainedProgram
      assert (null retainedDemand && any (\global -> globalIdentity global == sourceRoot
          && globalRequiredGeneration global == Just 11) (programGlobals retainedProgram))
        "root growth crossed a retained-generation original boundary"
      -- Grow an existing owner in reverse root order. Its enlarged body set
      -- must replace the old facts and retain both executable definitions.
      let sndSymbol = (preparedRootIdentity fstRoot) { symbolOccurrence = "snd" }
      sndRoots <- resolve [sndSymbol]
      small <- growPreparedRecovery initial sndRoots
      (expanded, expandedCount) <- measurePreparations (growPreparedRecovery small [fstRoot])
      let sameOwnerRoots = Set.toAscList (Set.fromList [sndSymbol, preparedRootIdentity fstRoot])
      expandedProgram <- project sameOwnerRoots (preparedRecoveryClosure expanded)
      sameOwnerBinders <- resolve sameOwnerRoots
      restartSameOwner <- newPreparedRecoveryWithPackageRoots hsc cache ownerCache bodyCache
        fixtureContext [subset] sameOwnerBinders
      restartedSameOwner <- preparedRecoveryClosure <$> restartSameOwner entry
      restartedProgram <- project sameOwnerRoots restartedSameOwner
      assert (expandedCount > 0) "larger exact body set did not reprepare its owner"
      assert (expandedProgram == restartedProgram)
        "same-owner root growth changed canonical output or reused stale body facts"
      -- Use genuine compiler-resolved roots to drive the complete recovery
      -- ledger/frontier. Each prefix is compared with a separate target state
      -- and a projection that recomputes reachability from all retained units.
      -- The request caches are shared here; the controls above separately prove
      -- cold execution and settlement rather than only cache-hit assembly.
      let historyRoots = [textRoot, fstRoot, head sndRoots]
          rootSymbols indices = Set.toAscList (Set.fromList
            [preparedRootIdentity (historyRoots !! index) | index <- indices])
          recompute roots closed = either (fail . show) (pure . fst) $
            prepareProjection (fixtureContext { projectionAuxiliaryRoots = roots })
              (closureModules closed) >>= projectSelected
      forM_ [1,2] $ \jobCount -> do
        grant <- either fail pure (compilerExecutionGrant jobCount)
        withCompilerExecutor grant $ \executor -> do
          setEnv "TIDEPOOL_RECOVERY_CHECK" "1"
          historyFactory <- newPreparedRecoveryWithDemand (Just executor) (\_ _ _ -> pure [])
            hsc cache ownerCache bodyCache fixtureContext [subset] []
          let checkPrefixes _ _ [] = pure True
              checkPrefixes current seen (index:rest) = do
                let nextSeen = seen ++ [index]
                    roots = rootSymbols nextSeen
                grown <- growPreparedRecovery current [historyRoots !! index]
                binders <- resolve roots
                restart <- newPreparedRecoveryWithPackageRoots hsc cache ownerCache bodyCache
                  fixtureContext [subset] binders
                restarted <- restart entry
                let closed = preparedRecoveryClosure grown
                    reference = preparedRecoveryClosure restarted
                selected <- project roots closed
                restartedProgram <- project roots reference
                recomputed <- recompute roots closed
                later <- checkPrefixes grown nextSeen rest
                pure (selected == restartedProgram && selected == recomputed
                  && closureFailures closed == closureFailures reference && later)
          result <- QC.quickCheckWithResult QC.stdArgs { QC.maxSuccess=24, QC.maxSize=6 }
            (QC.forAllShrink (QC.listOf1 (QC.elements [0,1,2])) (QC.shrinkList (const [])) $
              \indices -> QC.classify (length indices > Set.size (Set.fromList indices)) "repeated root" $
                QC.classify (Set.size (Set.fromList indices) > 1) "owner growth" $
                QC.counterexample ("recovery root history " ++ show indices) $
                QC.ioProperty (historyFactory entry >>= \start -> checkPrefixes start [] indices))
          unless (QC.isSuccess result) (fail "full recovery ledger history differs from recomputation")
      -- Calibrate the oracle against omission of a newly demanded real root.
      staleProgram <- recompute [preparedRootIdentity fstRoot] (preparedRecoveryClosure initial)
      completeFst <- growPreparedRecovery initial [fstRoot]
      completeProgram <- recompute [preparedRootIdentity fstRoot] (preparedRecoveryClosure completeFst)
      assert (staleProgram /= completeProgram)
        "full recovery history oracle missed omission of a demanded component"
      -- A failed exact Name remains attempted; growth cannot substitute a
      -- compatible lookup later and erase its original type-mismatch witness.
      bad <- growPreparedRecovery initial [setIdType fstRoot boolTy]
      (retried, retriedCount) <- measurePreparations (growPreparedRecovery bad [fstRoot])
      assert (any (\failure -> case failure of
          IncompatibleImplementation name _ -> name == varName fstRoot
          _ -> False) (closureFailures (preparedRecoveryClosure bad)))
        "wrong-typed root did not retain an exact lookup failure"
      assert (closureFailures (preparedRecoveryClosure retried)
          == closureFailures (preparedRecoveryClosure bad)
          && retriedCount == 0)
        "root growth rescued a failed exact lookup"
      putStrLn ("original package recovery: two expansions; preparation calls old="
        ++ show oldCount ++ ", continued=" ++ show newCount)

    -- Count the existing diagnostic owner instead of widening recovery's
    -- result contract for instrumentation. The suite executes sequentially.
    measurePreparations action = do
      (result, counters) <- measureRecoveryCounts action
      let key = "prepared_recover_module_preparations"
      assert (Map.member key counters) "recovery emitted no preparation count"
      pure (result, Map.findWithDefault 0 key counters)

    measureRecoveryCounts action = do
      setEnv "TIDEPOOL_TIMING" "1"
      bracket (openTempFile "/tmp" "tidepool-recovery-count")
        (\(path, handle) -> hClose handle >> removeFile path) $ \(_, handle) -> do
          result <- bracket (hDuplicate stderr) hClose $ \saved -> do
            hDuplicateTo handle stderr
            action `finally` (hFlush stderr >> hDuplicateTo saved stderr)
          hFlush handle
          hSeek handle AbsoluteSeek 0
          diagnostics <- hGetContents handle
          _ <- evaluate (length diagnostics)
          let counts = Map.fromListWith (+)
                [(name, read count :: Integer)
                | line <- lines diagnostics
                , let fields = words line
                , nameField <- fields, Just name <- [stripPrefix "name=" nameField]
                , isJust (stripPrefix "prepared_recover_" name)
                , countField <- fields, Just count <- [stripPrefix "count=" countField]]
          pure (result, counts)

    topIsFst (binding, _) = any
      ((== "fst") . occNameString . nameOccName . varName)
      (topBinders binding)

    namedResidual (UnsupportedExternalCapability name) =
      occNameString (nameOccName name) == "()"
    namedResidual _ = False

    assertNullaryRecoveryProjection context closure = case
      projectPreparedTarget context (closureModules closure) of
        Left failure -> ioError (userError
          ("recovered caller projection rejected the nullary constructor: " ++ show failure))
        Right program -> do
          let nullaryTops =
                [ binding
                | group <- programBindings program
                , TopBinding symbol binding <- groupItems group
                , symbolNamespace symbol == Text.pack "value"
                , symbolOccurrence symbol == Text.pack "()"
                ]
              nullaryGlobals =
                [ globalIdentity global
                | global <- programGlobals program
                , symbolOccurrence (globalIdentity global) == Text.pack "()"
                ]
          assert (length nullaryTops == 1)
            ("recovered projection did not intern exactly one () object: " ++ show nullaryTops)
          assert (all fieldFree nullaryTops)
            "recovered projection emitted a non-field-free () object"
          assert (null nullaryGlobals)
            ("recovered projection leaked () as an imported global: " ++ show nullaryGlobals)
      where
        groupItems (NonRecursive item) = [item]
        groupItems (Recursive items) = items
        fieldFree (HeapBinding _ (Constructor _ [])) = True
        fieldFree _ = False

    originalModuleRetained original recovered =
      any ((== pmModule original) . pmModule) recovered

    incompleteSubsetContract home context = do
      homeOtherEntry <- case
        [ identity
        | identity <- either (error . show) id (preparedTopIdentities [home])
        , symbolOccurrence identity == Text.pack "homeOther"
        ] of
        [identity] -> pure identity
        found -> ioError (userError ("expected one homeOther entry, got " ++ show found))
      let subset = home
            { preparedCoverage = ExactBodySubset
            , preparedBindings = filter hasHomeOther (pmBindings home)
            }
          subsetContext = context { projectionEntry = homeOtherEntry }
      case projectPreparedTarget subsetContext [subset] of
        Left failure -> ioError (userError
          ("incomplete subset rejected its genuine external global: " ++ show failure))
        Right program -> assert
          (any ((== Text.pack "homeValue") . symbolOccurrence . globalIdentity)
            (programGlobals program))
          "incomplete subset dropped its unresolved homeValue global"
      let complete = subset { preparedCoverage = CompleteSourceModule }
      case projectPreparedTarget subsetContext [complete] of
        Left (MissingPreparedTop _) -> pure ()
        Left failure -> ioError (userError
          ("complete source reported the wrong missing-top failure: " ++ show failure))
        Right _ -> ioError (userError
          "complete source with a missing top unexpectedly projected")
      where
        hasHomeOther (Stg.StgTopLifted binding, _) =
          any ((== "homeOther") . occNameString . nameOccName . varName)
            (bindingBinders binding)
        hasHomeOther _ = False

    topBinders (Stg.StgTopStringLit binder _) = [binder]
    topBinders (Stg.StgTopLifted binding) = bindingBinders binding

    bindingBinders (Stg.StgNonRec binder _) = [binder]
    bindingBinders (Stg.StgRec pairs) = map fst pairs

verifyRecoveryPublication :: HscEnv -> ProjectionContext -> [PreparedModule] -> RecoveredClosure -> IO ()
verifyRecoveryPublication env context modules original = do
  requirePreparedRecoveryPublication "caller" original
  root <- case [binder | binder <- preparedTargetReferences context modules
    , occNameString (nameOccName (varName binder)) == "fst"] of
      [binder] -> pure binder
      found -> fail ("expected one genuine fst recovery input, got " ++ show (length found))
  owner <- maybe (fail "genuine fst input has no defining owner") pure
    (nameModule_maybe (varName root))
  (_,location) <- readExactInterface env owner >>= either (fail . show) pure
  bracket (openTempFile "/tmp" "tidepool-recovery-broken-interface")
    (\(path,handle) -> hClose handle >> removeFile path) $ \(path,handle) -> do
      hPutStr handle "not-a-ghc-interface"
      hFlush handle
      hClose handle
      finder <- initFinderCache
      addModuleToFinder finder (GWIB owner NotBoot)
        location {ml_hi_file=path,ml_dyn_hi_file=path}
      let broken = env {hsc_FC=finder}
      forM_ [False,True] $ \preparedInterface -> do
        cache <- newFatIfaceCache
        when preparedInterface $ do
          body <- lookupFatIfaceExact env cache (varName root)
          case body of
            FatIfaceFound _ -> pure ()
            _ -> fail "genuine fst defining body could not seed the fault history"
        owners <- newOwnerInterfaceCache
        bodies <- newPreparedBodyCache
        failed <- recoverPreparedClosure broken cache owners bodies context modules
        let failures = closureFailures failed
            relevant failure = case failure of
              InterfaceLoadingFailure selected _ -> not preparedInterface && selected == owner
              DefiningPreparationFailure (RecoveredModuleInterfaceFailure selected _) ->
                preparedInterface && selected == owner
              _ -> False
        assert (any relevant failures)
          ("corrupt defining interface lost its typed recovery failure: " ++ show failures)
        published <- try (requirePreparedRecoveryPublication "caller" failed)
        case published of
          Left (RecoveryPublicationFailure target retained) ->
            assert (target == "caller" && any relevant retained)
              "publication did not retain its genuine defining failure"
          Right () -> fail "publication accepted a corrupt defining interface"
        assert (closureFailures failed == failures) "publication changed raw corpus residuals"
      pure ()
  -- These typed semantic residuals remain diagnostic evidence. They do not
  -- claim that the genuine fst body has any of these unsupported properties.
  let semantic = original {closureFailures =
        [ MissingImplementation (varName root) NoExtraDeclarations
        , UnsupportedExternalCapability (varName root)
        , IncompatibleImplementation (varName root) "type refusal"
        , MissingHomeImplementation (varName root) ]}
  requirePreparedRecoveryPublication "caller" semantic

assertOverlapMerge :: IO ()
assertOverlapMerge = do
  let a = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991001) (mkVarOcc "a")) intTy
      b = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991002) (mkVarOcc "b")) intTy
      c = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991003) (mkVarOcc "c")) intTy
      d = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991004) (mkVarOcc "d")) intTy
      e = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991005) (mkVarOcc "e")) intTy
      previous = [ Rec [(a, Var b), (b, Var a)]
                 , Rec [(c, Var d), (d, Var c)] ]
      incoming = Rec [(a, Var c), (c, Var a), (e, Var e)]
      merged = insertGroup incoming previous
  case merged of
    [Rec pairs] -> assert (all (`elem` map (occNameString . nameOccName . varName . fst) pairs)
      ["a", "b", "c", "d", "e"])
      ("overlapping merge dropped a sibling: " ++ show (map (occNameString . nameOccName . varName . fst) pairs))
    other -> ioError (userError ("overlapping groups were not merged: " ++ show (length other)))

assertSubsetPreservesFullGroup :: IO ()
assertSubsetPreservesFullGroup = do
  let a = mkVanillaGlobal (mkSystemName (mkUnique 'b' 992001) (mkVarOcc "a")) intTy
      b = mkVanillaGlobal (mkSystemName (mkUnique 'b' 992002) (mkVarOcc "b")) intTy
      c = mkVanillaGlobal (mkSystemName (mkUnique 'b' 992003) (mkVarOcc "c")) intTy
      existing = [Rec [(a, Var b), (b, Var a)]]
      incoming = NonRec a (Var c)
  case insertGroup incoming existing of
    [Rec pairs] -> do
      assert (map (occNameString . nameOccName . varName . fst) pairs == ["a", "b"])
        "subset lookup dropped a sibling from the full Rec group"
      case [rhs | (binder, rhs) <- pairs, varName binder == varName a] of
        [Var reference] -> assert (varName reference == varName b)
          "subset lookup replaced the authoritative full-group body"
        _ -> ioError (userError "subset lookup did not retain an a body")
    other -> ioError (userError
      ("subset lookup changed the full Rec group shape: " ++ show (length other)))
