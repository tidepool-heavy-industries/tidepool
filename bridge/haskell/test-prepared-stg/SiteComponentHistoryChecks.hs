module SiteComponentHistoryChecks (verifySiteComponentHistories) where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Data.IntMap.Strict qualified as IntMap
import Data.List (sortOn)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import GHC
import GHC.Driver.Session (gopt_set, updOptLevel)
import GHC.Types.Name (nameModule_maybe)
import GHC.Types.Name.Occurrence (occNameString)
import System.Directory (copyFile, createDirectoryIfMissing)
import System.FilePath ((</>))
import System.Process (readProcess)
import Tidepool.EffectSchema (ysSite)
import Tidepool.ExecutionProjection qualified as Projection
import Tidepool.ExecutionSchema qualified as Wire
import Tidepool.FatIface
import Tidepool.PreparedStg

assert :: Bool -> String -> IO ()
assert condition message = unless condition (fail message)

-- Real interfaces issue both the private components and their changing site
-- authority. Neither prepared units nor consumed witnesses are fabricated.
verifySiteComponentHistories :: FilePath -> IO ()
verifySiteComponentHistories directory = do
  let actor = directory </> "Tidepool/Actor.hs"
      scope = directory </> "Tidepool/Scope/Internal.hs"
      source = directory </> "SiteComponentUse.hs"
      fixture name target = copyFile ("test-prepared-stg/site-fixtures" </> name) target
  createDirectoryIfMissing True (directory </> "Tidepool/Internal")
  createDirectoryIfMissing True (directory </> "Tidepool/Scope")
  copyFile "lib/Tidepool/Internal/RequestSite.hs" (directory </> "Tidepool/Internal/RequestSite.hs")
  fixture "TypedPreparationActor.hs" actor
  fixture "SiteComponentScope.hs" scope
  fixture "SiteComponentFixture.hs" (directory </> "SiteComponentFixture.hs")
  fixture "SiteComponentUse.hs" source
  libdir <- takeWhile (/= '\n') <$> readProcess "ghc" ["--print-libdir"] ""
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags
      ((`gopt_set` Opt_WriteInterface) $ (`gopt_set` Opt_WriteIfSimplifiedCore)
       $ (`gopt_set` Opt_ForceRecomp) $ updOptLevel 1 flags)
        { importPaths = directory : importPaths flags
        , hiDir = Just directory, objectDir = Just directory }
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    let reload = do
          loaded <- load LoadAllTargets
          case loaded of Failed -> fail "site component fixture did not compile"; Succeeded -> pure ()
          getSession
    environment <- reload
    info <- getModuleInfo =<< findModule (mkModuleName "SiteComponentFixture") Nothing
    names <- maybe (fail "site fixture has no module info") (pure . modInfoExports) info
    let root occurrence = case filter ((== occurrence) . occNameString . nameOccName) names of
          [name] -> name
          _ -> error ("missing genuine site root " ++ occurrence)
        rootA = root "answerA"
        rootB = root "answerB"
    (fat, owners, bodies, selectionA, selectionAB, first, grown) <- liftIO $ do
      fat <- newFatIfaceCache
      owners <- newOwnerInterfaceCache
      bodies <- newPreparedBodyCache
      let select roots = lookupFatIfaceComponents environment fat
            (maybe (error "site root has no defining owner") id (nameModule_maybe rootA)) roots >>= \case
              FatIfaceComponents selected -> pure selected
              other -> fail ("genuine site component selection failed: " ++ describe other)
      selectionA <- select [rootA]
      selectionAB <- select [rootA,rootB]
      assert (length (fatSelectionComponents selectionA) == 1
          && length (fatSelectionComponents selectionAB) == 2)
        "site fixture did not produce two distinct true private components"
      acquire <- newPreparedComponentUnitsPreparer environment owners bodies
      firstTask <- acquire Nothing selectionA >>= requireRight
      first <- runPreparedComponentTask firstTask >>= requireRight
      grownTask <- acquire (Just first) selectionAB >>= requireRight
      assert (length (preparedComponentTaskPending grownTask) == 1)
        "A to A+B scheduled a completed component or its shared workers"
      grown <- runPreparedComponentTask grownTask >>= requireRight
      validatePreparedComponents grown
      assertPreserved "growth replaced A or its shared workers" first grown
      let modules = preparedComponentModules grown
          sites = concatMap pmYieldSites modules
          internalTops = Map.unions (map pmStableTopSpellings modules)
          originalNames = Set.fromList (map varName (fatComponentAllBinders
            (head (fatSelectionComponents selectionAB))))
      assert (length sites == 2 && Set.size (Set.fromList (map ysSite sites)) == 2
          && all (null . pmSiteRejections) modules)
        "separate component arenas lost or duplicated a genuine typed site"
      assert (not (Map.null (Map.withoutKeys internalTops originalNames)))
        "site fixture did not exercise genuinely new emitted tops"
      let groups = IntMap.elems (IntMap.unions
            (map fatComponentBindings (fatSelectionComponents selectionAB)))
          owner = fatOriginalOwner (fatSelectionVersion selectionAB)
      eagerOwners <- newOwnerInterfaceCache
      eagerBodies <- newPreparedBodyCache
      eager <- prepareRecoveredBodies environment eagerOwners eagerBodies owner groups >>= requireRight
      incremental <- requireRight (project modules)
      reference <- requireRight (project [eager])
      assert (Wire.programTypes incremental == Wire.programTypes reference
          && sortOn Wire.siteId (Wire.programSites incremental) == sortOn Wire.siteId (Wire.programSites reference))
        "per-component site evidence differs from independently prepared eager evidence"
      pure (fat,owners,bodies,selectionA,selectionAB,first,grown)
    -- A consumed sibling changes only its actual component variant. Keep the
    -- original selected Core and defining-context owner throughout the history.
    liftIO (appendFile scope "\nversion :: Int\nversion = 23\n")
    changedB <- reload
    replacedB <- liftIO $ do
      acquire <- newPreparedComponentUnitsPreparer changedB owners bodies
      task <- acquire (Just grown) selectionAB >>= requireRight
      assert (length (preparedComponentTaskPending task) == 1)
        "changed B authority broadly invalidated A or reused stale B"
      current <- runPreparedComponentTask task >>= requireRight
      assertPreserved "changed B authority replaced A" first current
      assertReplaced "changed B authority retained the old actual unit" grown current
      pure current
    liftIO (appendFile actor "\nversion :: Int\nversion = 29\n")
    changedA <- reload
    liftIO $ do
      acquire <- newPreparedComponentUnitsPreparer changedA owners bodies
      task <- acquire (Just replacedB) selectionAB >>= requireRight
      assert (length (preparedComponentTaskPending task) == 1)
        "changed A authority broadly invalidated B or reused stale A"
      current <- runPreparedComponentTask task >>= requireRight
      assertReplaced "changed A authority retained the old actual unit" replacedB current
      let firstKeys = Map.keysSet (preparedComponentUnitRows first)
          retainedB = Map.withoutKeys (preparedComponentUnitRows replacedB) firstKeys
      assert (all (\(key,unit) -> maybe False (samePreparedUnit unit)
          (Map.lookup key (preparedComponentUnitRows current))) (Map.toList retainedB))
        "changed A authority replaced the already refreshed B variant"
      validatePreparedComponents current
      -- The original exact selection can still be queried; no roster cache or
      -- alternate compiler issuer was introduced by the history itself.
      assert (fatSelectionVersion selectionA == fatSelectionVersion selectionAB)
        "growth changed the captured original version"
      _ <- lookupFatIfaceComponents changedA fat (fatOriginalOwner (fatSelectionVersion selectionA)) [rootA]
      pure ()
  where
    requireRight :: Show failure => Either failure value -> IO value
    requireRight = either (fail . show) pure
    describe FatIfaceComponents{} = "ready"
    describe (FatIfaceComponentsMissing reason) = show reason
    describe (FatIfaceComponentsLoadFailure _ reason) = reason
    assertPreserved message previous current = assert
      (all (\(key,unit) -> maybe False (samePreparedUnit unit)
        (Map.lookup key (preparedComponentUnitRows current)))
        (Map.toList (preparedComponentUnitRows previous))) message
    assertReplaced message previous current = assert
      (length [() | (key,unit) <- Map.toList (preparedComponentUnitRows previous)
        , Just next <- [Map.lookup key (preparedComponentUnitRows current)]
        , not (samePreparedUnit unit next)] == 1) message
    project modules = Projection.projectPreparedTarget
      Projection.ProjectionContext
        { Projection.projectionProfile = "ghc-9.12-prepared-stg"
        , Projection.projectionToolchain = "ghc-9.12.2"
        , Projection.projectionTarget = Wire.TargetDescriptor Wire.X86_64 Wire.LittleEndian 64 64 "sysv64" []
        , Projection.projectionRetainedGenerations = mempty
        , Projection.projectionCurrentOriginals = mempty
        , Projection.projectionEntry = Wire.SymbolIdentity "main" "SiteComponentFixture" "value" "answerA" Nothing
        , Projection.projectionAuxiliaryRoots = [Wire.SymbolIdentity "main" "SiteComponentFixture" "value" "answerB" Nothing]
        , Projection.projectionFormattingAuthority = Nothing
        , Projection.projectionTimeAuthority = Nothing
        , Projection.projectionJsonAuthority = Nothing
        , Projection.projectionTextUnit = Nothing } modules
