module TypedSessionCases
  ( typedSessionHydrationPublicationChecks, typedSessionPrefixProperties ) where

import Control.Exception
  ( AsyncException(ThreadKilled), Exception, IOException, SomeException, bracket, onException, throwIO, try )
import Control.Monad (forM, forM_, unless, void)
import qualified Crypto.Hash.SHA256 as SHA256
import Data.Bits (testBit)
import Data.IORef (newIORef, readIORef, writeIORef)
import Data.List (sort)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Text as T
import GHC (getSessionDynFlags, runGhc)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.Type (liftedTypeKind, mkInfForAllTy, mkInfForAllTys, mkTyVarTy, mkVisFunTyMany)
import GHC.Driver.Env (HscEnv, hsc_HPT, hsc_home_unit)
import GHC.Types.Fixity (Fixity(..), FixityDirection(..))
import GHC.Types.Id (Id, idName, idType)
import GHC.Types.Name (mkInternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (mkTyVarOcc, mkVarOcc, occNameString)
import GHC.Types.PkgQual (PkgQual(NoPkgQual))
import GHC.Types.SrcLoc (noSrcSpan)
import GHC.Types.Unique.Supply (mkSplitUniqSupply, takeUniqFromSupply)
import GHC.Types.Var (mkTyVar, varName)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Home.ModInfo (lookupHpt, hm_iface)
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Module.ModIface (mi_fixities, mi_extra_decls)
import GHC.Unit.Types (unitString)
import Numeric (showHex)
import System.Directory
  ( createDirectory, createDirectoryIfMissing, doesPathExist
  , getTemporaryDirectory, pathIsSymbolicLink, removeDirectoryRecursive, removeFile )
import System.FilePath ((</>), takeDirectory)
import System.IO (openTempFile, hClose)
import System.IO.Error (isAlreadyExistsError)
import System.Posix.Files (createSymbolicLink, readSymbolicLink)
import Test.Tasty (TestTree, testGroup, withResource)
import Test.Tasty.HUnit (testCase)
import Test.Tasty.QuickCheck (testProperty)
import qualified Test.QuickCheck as QC
import Tidepool.Binders
  ( BoundBinder(..), ValueTier(..), CellSplitError(..)
  , analyzeCellWithFlags, analyzeOrderedCellWithFlags
  , prepareTypedSegmentSource, preparedTypedSegmentPlan
  , preparedTypedSegmentSource, preparedTypedSegmentOperations )
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.DependencyEvidence (DependencyEvidence(..))
import Tidepool.ExactHydration
  ( newOriginalInterfaceArtifactsWithSessionOutputs )
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, captureFinalizedModuleArtifacts
  , finalizedValueInterfaceSeals, finalizedInterfaceSeals )
import Tidepool.CheckedCell (captureCheckedSignature, resolveCheckedSignature)
import Tidepool.GhcPipeline
  ( PipelineSelection(..), CompilePurpose(..), withResidentPipelineSelectedRequests
  , preparedSegmentCaptures )
import Tidepool.Identity (stableVarId)
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..), SessionScope(..)
  , CapturedSessionInterface, capturedSessionInterface, capturedSessionInterfaceEvidence
  , injectSessionIfaceWithBindings, injectSessionScopeWithCaptures
  , mkThinSessionIface, renderSessionModule, sessionHiPath, writeSessionIface )
import Tidepool.SessionArtifacts
import Tidepool.Test.Runner (requiredInput)
import Tidepool.TypedSegment
  ( PendingTypedSegment
  , typedSegmentItems, pendingSegmentItems, typedItemCaptures, typedCaptureIdentifier
  , typedCaptureType, typedCaptureFixity, typedItemInputs, typedInputCapture
  , typedItemPlan, TypedItemPlan(..) )

data SessionBoundaryFailure = HydrationCompletionRefused | PublicationCompletionRefused
  deriving (Eq, Show)
instance Exception SessionBoundaryFailure

type InterfaceSeal = ((T.Text, T.Text), T.Text)

data PrefixAdmission = PrefixAdmission
  FinalizedModuleArtifacts [CapturedSessionInterface] [CapturedSessionInterface]

data PrefixFixture = PrefixFixture FilePath PreparedTypedSegmentBindings
  [(Int, InterfaceSeal)] (Map.Map Int PrefixAdmission)

data PrefixHistory = PrefixHistory Int [Int] deriving Show

-- Compile once, then compare the prepared capture selection with authored
-- reservation ordinals and independently hashed interface bytes. Receipt
-- admission and future-value refusals belong to the Rust consumer cluster.
typedSessionPrefixProperties :: TestTree
typedSessionPrefixProperties = withResource acquirePrefixFixture releasePrefixFixture $ \fixture ->
  testGroup "typed-session-prefix"
    [ testCase "delayed capture stays unavailable before its item" $ do
        value@(PrefixFixture _ prepared rows admissions) <- fixture
        let admission = admissions Map.! 0
        early <- issuePrefix prepared admission 1
        unless (sort early == sort (expectedPrefix value 0 1))
          (fail "item 1 published the future Val.G4 capture")
        later <- issuePrefix prepared admission 5
        unless (sort later == sort (map snd rows))
          (fail "later reader did not retain its delayed capture interface")
        -- Returning the complete output inventory at item 1 would violate
        -- prefix authority and fails this same observation.
        unless (sort (map snd rows) /= sort (expectedPrefix value 0 1))
          (fail "prefix oracle cannot distinguish future-output publication")
    , testProperty "prepared capture selections preserve exact roles and immutable facts" $
        QC.checkCoverage $
        QC.forAllShrink prefixHistoryGenerator shrinkPrefixHistory $ \history@(PrefixHistory mask queries) ->
          QC.cover 10 (mask == 0) "all produced"
          $ QC.cover 10 (mask == 7) "all imported"
          $ QC.cover 60 (mask > 0 && mask < 7) "mixed roles"
          $ QC.cover 30 (any (<= 2) queries) "before delayed capture"
          $ QC.cover 30 (any (>= 5) queries) "later reader"
          $ QC.ioProperty $ do
              value@(PrefixFixture _ prepared rows admissions) <- fixture
              let admission@(PrefixAdmission full _ _) = admissions Map.! mask
              projected <- mapM (issuePrefix prepared admission) queries
              pure $ QC.counterexample (show history) $ QC.conjoin
                ( [sort issued QC.=== sort (expectedPrefix value mask ordinal)
                  | (ordinal, issued) <- zip queries projected]
                  ++ [sort (finalizedInterfaceSeals full) QC.=== sort (map snd rows)] )
    ]

prefixHistoryGenerator :: QC.Gen PrefixHistory
prefixHistoryGenerator = do
  mask <- QC.chooseInt (0, 7)
  count <- QC.chooseInt (1, 8)
  PrefixHistory mask <$> QC.vectorOf count (QC.chooseInt (-1, 6))

shrinkPrefixHistory :: PrefixHistory -> [PrefixHistory]
shrinkPrefixHistory (PrefixHistory mask queries) =
  [PrefixHistory smaller queries | smaller <- QC.shrink mask, smaller >= 0, smaller <= 7]
    ++ [PrefixHistory mask smaller | smaller <- QC.shrinkList shrinkOrdinal queries, not (null smaller)]
  where shrinkOrdinal ordinal = filter (\n -> n >= -1 && n <= 6) (QC.shrink ordinal)

expectedPrefix :: PrefixFixture -> Int -> Int -> [InterfaceSeal]
expectedPrefix (PrefixFixture _ _ rows _) mask ordinal =
  [row | (index, (captureOrdinal, row)) <- zip [0 ..] rows
    , testBit mask index || captureOrdinal <= ordinal]

issuePrefix :: PreparedTypedSegmentBindings -> PrefixAdmission -> Int -> IO [InterfaceSeal]
issuePrefix prepared (PrefixAdmission full imported produced) ordinal = do
  let selected = map interfaceOwner imported ++
        [interfaceOwner snapshot | snapshot <- typedSegmentSessionInterfacesThrough ordinal prepared
          , interfaceOwner snapshot `elem` map interfaceOwner produced]
  pure [row | row@(owner, _) <- finalizedValueInterfaceSeals full, owner `elem` selected]

interfaceOwner :: CapturedSessionInterface -> (T.Text, T.Text)
interfaceOwner snapshot = let (owner, _) = capturedSessionInterface snapshot
  in (T.pack (unitString (moduleUnit owner)), T.pack (moduleNameString (moduleName owner)))

releasePrefixFixture :: PrefixFixture -> IO ()
releasePrefixFixture (PrefixFixture root _ _ _) = removeDirectoryRecursive root

acquirePrefixFixture :: IO PrefixFixture
acquirePrefixFixture = do
  root <- temporary
  prepare root `onException` removeDirectoryRecursive root
  where
    prepare root = do
      effects <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
      prelude <- requiredInput "TIDEPOOL_PRELUDE_DIR"
      body <- readFile "test-cell-splitter/fixtures/typed-session/TypedSessionDelayedCapture.hs"
      libdir <- getLibdir
      flags <- runGhc (Just libdir) getSessionDynFlags
      let target = root </> "TypedSessionDelayedCapture.hs"
          includes = [root, effects, prelude]
          template = unlines
            [ "{-# LANGUAGE DataKinds #-}"
            , "{{CELL_PRAGMAS}}"
            , "module TypedSessionDelayedCapture where"
            , "import Control.Monad.Freer (Eff)"
            , "import Tidepool.Effects.Core ()"
            , "{{CELL_IMPORTS}}"
            , "{{CELL_DECLS}}"
            , "__tidepool_cell_check :: Eff '[] ()"
            , "__tidepool_cell_check = do { {{CELL_BODY}} ; pure () }"
            ]
          expectedCaptures = [(0, ["first"]), (1, []), (2, []), (3, ["delayed"]), (4, []), (5, ["later"])]
          reservations = [(ordinal, fromIntegral (ordinal + 1), Nothing) | ordinal <- [0 .. 5]]
      sourcePlan <- analyzeOrderedCellWithFlags flags template body >>= either (fail . show) pure
      source <- either fail pure (prepareTypedSegmentSource template sourcePlan (replicate 64 'd') reservations)
      writeFile target (preparedTypedSegmentSource source)
      prepared <- withResidentPipelineSelectedRequests includes $ \runRequest -> do
        observed <- newIORef Nothing
        let complete initial admitted typed = do
              let items = pendingSegmentItems typed
                  actual = [(plannedItemOrdinal (typedItemPlan item), map (occurrence . typedCaptureIdentifier)
                    (typedItemCaptures item)) | item <- items]
              unless (actual == expectedCaptures)
                (fail ("delayed-prefix fixture changed its capture premises: " ++ show actual))
              let delayed = typedCaptureIdentifier (head (typedItemCaptures (items !! 3)))
              unless (delayed `elem` map typedInputCapture (typedItemInputs (items !! 5)))
                (fail "later reader does not depend on the original delayed capture")
              batch <- prepareTypedSegmentSessionBindings initial admitted typed (root </> "stage")
              let snapshots = typedSegmentSessionInterfaces batch
                  unit = T.pack (unitString (homeUnitAsUnit (hsc_home_unit initial)))
                  owners = [(unit, T.pack (moduleNameString (renderSessionModule (SessionModule ValMod (Generation generation)))))
                    | generation <- [1, 4, 6]]
              unless (map interfaceOwner snapshots == owners)
                (fail "delayed-prefix fixture lost its exact compiler home-unit/output owners")
              writeIORef observed (Just batch)
              pure (typedSegmentSessionEnvironment batch, typedSegmentSessionGlobals batch,
                typedSegmentSessionInterfaces batch)
        void $ runRequest (pure ()) $ \compiler -> compiler
          (WithTypedSegmentPreparation complete (PreparedSegmentProducts (preparedTypedSegmentPlan source) Nothing))
          mempty (TypedSegmentCompile (preparedTypedSegmentPlan source) (preparedTypedSegmentOperations source) GeneralCompile)
          Nothing target includes Nothing
        readIORef observed >>= maybe (fail "delayed-prefix preparation callback did not execute") pure
      let snapshots = typedSegmentSessionInterfaces prepared
          rows = zip [0, 3, 5] [ (interfaceOwner snapshot, digest (snd (capturedSessionInterface snapshot)))
            | snapshot <- snapshots ]
          environment = typedSegmentSessionEnvironment prepared
          evidence = DependencyEvidence False False [] [] [] []
      admissions <- forM [0 .. 7] $ \mask -> do
        let imported = [snapshot | (index, snapshot) <- zip [0 ..] snapshots, testBit mask index]
            produced = [snapshot | (index, snapshot) <- zip [0 ..] snapshots, not (testBit mask index)]
            directory = root </> "roles-" ++ show mask
        createDirectoryIfMissing True directory
        originals <- newOriginalInterfaceArtifactsWithSessionOutputs environment Map.empty [] imported produced directory
        full <- captureFinalizedModuleArtifacts originals environment Map.empty Map.empty evidence directory
        unless (sort (finalizedInterfaceSeals full) == sort (map snd rows))
          (fail "real role issuer changed an exact output owner or interface digest")
        pure (mask, PrefixAdmission full imported produced)
      pure (PrefixFixture root prepared rows (Map.fromList admissions))
    digest = T.pack . concatMap hex . BS.unpack . SHA256.hash
    hex byte = let rendered = showHex byte "" in replicate (2 - length rendered) '0' ++ rendered

-- Every negative follows a real compiler/capture/hydration positive. The
-- protected fixture's local fixity is an internal GHC component control,
-- not an extension of the resident cell parser's accepted syntax.
typedSessionHydrationPublicationChecks :: IO ()
typedSessionHydrationPublicationChecks = bracket temporary removeDirectoryRecursive $ \root -> do
  effects <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  prelude <- requiredInput "TIDEPOOL_PRELUDE_DIR"
  body <- readFile "test-cell-splitter/fixtures/typed-session/TypedSessionCaptures.hs"
  libdir <- getLibdir
  flags <- runGhc (Just libdir) getSessionDynFlags
  let target = root </> "TypedSessionCaptures.hs"
      includes = [root, effects, prelude]
      firstOwner = SessionModule ValMod (Generation 701)
      secondOwner = SessionModule ValMod (Generation 702)
      owners = [firstOwner, secondOwner]
      liveRoot = root </> "live"
      template = unlines
        [ "{-# LANGUAGE DataKinds #-}"
        , "{{CELL_PRAGMAS}}"
        , "module TypedSessionCaptures where"
        , "import Control.Monad.Freer (Eff)"
        , "import Tidepool.Effects.Core ()"
        , "{{CELL_IMPORTS}}"
        , "{{CELL_DECLS}}"
        , "__tidepool_cell_check :: Eff '[] ()"
        , "__tidepool_cell_check = do { {{CELL_BODY}} ; pure () }"
        ]
  -- The existing standalone parser admits this internal GHC fixture. The
  -- resident ordered parser must still refuse local fixities across items.
  ordered <- analyzeOrderedCellWithFlags flags template body
  case ordered of
    Left (CellUnsupportedLocalFixity _) -> pure ()
    _ -> fail "Session component control changed the resident local-fixity boundary"
  sourcePlan <- analyzeCellWithFlags flags template body >>= either (fail . show) pure
  source <- either fail pure (prepareTypedSegmentSource template sourcePlan (replicate 64 'a')
    [(0, 701, Nothing), (1, 702, Nothing)])
  let plan = preparedTypedSegmentPlan source
      operations = preparedTypedSegmentOperations source
  writeFile target (preparedTypedSegmentSource source)
  withResidentPipelineSelectedRequests includes $ \runRequest -> do
    let acquire staging afterHydration = runRequest (pure ()) $ \compiler -> do
          observed <- newIORef Nothing
          let complete initial admitted typed = do
                prepared <- prepareTypedSegmentSessionBindings initial admitted typed staging
                verifyBatch owners typed prepared
                writeIORef observed (Just (initial, admitted, typed, prepared))
                afterHydration prepared
                pure (typedSegmentSessionEnvironment prepared,
                  typedSegmentSessionGlobals prepared, typedSegmentSessionInterfaces prepared)
          result <- compiler
            (WithTypedSegmentPreparation complete (PreparedSegmentProducts plan Nothing))
            mempty (TypedSegmentCompile plan operations GeneralCompile) Nothing target includes Nothing
          case typedSegmentItems (preparedSegmentCaptures result) of
            [_, _] -> pure ()
            _ -> fail "Session control did not receive two compiler-issued items"
          readIORef observed >>= maybe (fail "typed preparation callback did not execute") pure

    (initial, admitted, typed, prepared) <- acquire (root </> "positive-stage") (const (pure ()))
    verifyOutputProjection root prepared
    verifySigmaTransport initial (root </> "sigma-transport")
    forM_ owners (assertNoStagingFinder initial (root </> "positive-stage"))
    forM_ owners (assertNoStagingFinder (typedSegmentSessionEnvironment prepared) (root </> "positive-stage"))

    -- The first real interface hydrates successfully; corruption of the
    -- second file must be a decoder refusal, with no partial finder grant.
    let faultRoot = root </> "second-hydration-fault"
    copySnapshots faultRoot owners (typedSegmentSessionInterfaces prepared)
    (firstHydrated, firstIds, _) <- injectSessionIfaceWithBindings faultRoot firstOwner initial
    unless (length firstIds == 1) (fail "first hydration prerequisite is empty")
    BS.writeFile (sessionHiPath faultRoot secondOwner) (BS.pack [0, 1, 2])
    refused <- try (void (injectSessionIfaceWithBindings faultRoot secondOwner firstHydrated))
      :: IO (Either IOException ())
    case refused of Left _ -> pure (); Right _ -> fail "corrupt second interface hydrated"
    forM_ owners (assertNoStagingFinder initial faultRoot)
    forM_ owners (assertNoStagingFinder firstHydrated faultRoot)
    recovered <- prepareTypedSegmentSessionBindings initial admitted typed (root </> "decoder-retry")
    verifyBatch owners typed recovered

    -- Refusal and cancellation occur after both actual hydrations, inside
    -- the real compiler callback. Releasing the cancelled request owns
    -- interpreter recovery; a fresh request on the same resident compiles A.
    refusedCompletion <- try (void (acquire (root </> "hydrate-refused")
      (\_ -> throwIO HydrationCompletionRefused))) :: IO (Either SessionBoundaryFailure ())
    unless (refusedCompletion == Left HydrationCompletionRefused)
      (fail "hydration completion did not preserve its exact refusal")
    cancelledCompletion <- try (void (acquire (root </> "hydrate-cancelled")
      (\_ -> throwIO ThreadKilled))) :: IO (Either AsyncException ())
    unless (cancelledCompletion == Left ThreadKilled)
      (fail "hydration cancellation did not propagate")
    (_, _, retryTyped, retryPrepared) <- acquire (root </> "request-retry") (const (pure ()))
    verifyBatch owners retryTyped retryPrepared

    -- An unrelated real thin interface is already present. Neither a
    -- receipt refusal nor cancellation may remove or overwrite it.
    answerCapture <- case [capture | item <- pendingSegmentItems typed
      , capture <- typedItemCaptures item, occurrence (typedCaptureIdentifier capture) == "answer"] of
      [capture] -> pure capture
      _ -> fail "positive capture inventory has no unique answer"
    let prior = SessionModule ValMod (Generation 690)
        answerType = typedCaptureType answerCapture
        priorPath = sessionHiPath liveRoot prior
    priorIface <- mkThinSessionIface initial prior [(mkVarOcc "priorAnswer", answerType)]
    writeSessionIface initial liveRoot prior priorIface
    priorBytes <- BS.readFile priorPath
    let completePublication :: Exception failure => failure -> IO ()
        completePublication failure = do
          forM_ owners $ \owner -> forM_ (bindingPaths liveRoot owner) $ \path -> do
            exists <- doesPathExist path
            unless exists (fail "completion fault ran before all actual publication writes")
          throwIO failure
        assertRolledBack = do
          assertAbsent liveRoot owners
          bytes <- BS.readFile priorPath
          unless (bytes == priorBytes) (fail "rollback changed the preexisting interface")
    refusedPublication <- try (withTypedSegmentSessionPublication liveRoot retryPrepared
      (completePublication PublicationCompletionRefused)) :: IO (Either SessionBoundaryFailure ())
    unless (refusedPublication == Left PublicationCompletionRefused)
      (fail "publication did not preserve its completion refusal")
    assertRolledBack
    cancelledPublication <- try (withTypedSegmentSessionPublication liveRoot retryPrepared
      (completePublication ThreadKilled)) :: IO (Either AsyncException ())
    unless (cancelledPublication == Left ThreadKilled) (fail "publication cancellation did not propagate")
    assertRolledBack

    -- A colliding valid original output is refused without deleting its
    -- bytes or writing another member of the batch.
    let collisionRoot = root </> "collision"
    firstSnapshot <- case typedSegmentSessionInterfaces retryPrepared of
      first : _ -> pure first
      _ -> fail "retry has no first capture snapshot"
    copySnapshots collisionRoot [firstOwner] [firstSnapshot]
    collisionBytes <- mapM BS.readFile (bindingPaths collisionRoot firstOwner)
    collision <- try (withTypedSegmentSessionPublication collisionRoot retryPrepared (pure ()))
      :: IO (Either IOException ())
    case collision of Left _ -> pure (); Right _ -> fail "existing generation was overwritten"
    unchanged <- mapM BS.readFile (bindingPaths collisionRoot firstOwner)
    unless (unchanged == collisionBytes) (fail "collision refusal altered the previous generation")
    assertAbsent collisionRoot [secondOwner]

    -- A dangling directory entry is absent to the existence preflight but
    -- refuses the exclusive link for the second owner. The first owner's
    -- links must roll back, and the preexisting entry remains untouched.
    let writeFaultRoot = root </> "second-publication-write"
        blockedPath = sessionHiPath writeFaultRoot secondOwner
        missingTarget = writeFaultRoot </> "absent-interface"
    createDirectoryIfMissing True (takeDirectory blockedPath)
    createSymbolicLink missingTarget blockedPath
    writeRefused <- try (withTypedSegmentSessionPublication writeFaultRoot retryPrepared (pure ()))
      :: IO (Either IOException ())
    case writeRefused of
      Left failure | isAlreadyExistsError failure -> pure ()
      _ -> fail "publication did not refuse at the second owner's exclusive link"
    assertAbsent writeFaultRoot [firstOwner]
    retainedEntry <- pathIsSymbolicLink blockedPath
    unless retainedEntry (fail "rollback removed a preexisting directory entry")
    retainedTarget <- readSymbolicLink blockedPath
    unless (retainedTarget == missingTarget) (fail "rollback changed the preexisting link")
    forM_ [blockedPath ++ ".packages", blockedPath ++ ".requirements"] $ \path -> do
      exists <- doesPathExist path
      unless (not exists) (fail "second-owner refusal left partial sidecars")

    withTypedSegmentSessionPublication liveRoot retryPrepared (pure ())
    -- Discard the inputs after success. Future scope hydration must select
    -- the published root, with the same actual types and nondefault fixity.
    removeDirectoryRecursive (root </> "request-retry")
    (live, snapshots) <- injectSessionScopeWithCaptures
      (SessionScope liveRoot owners Nothing Nothing) initial
    unless (length snapshots == 2) (fail "retry lost a published interface")
    assertFixities live owners
    forM_ owners $ \owner -> do
      found <- findImportedModule live (renderSessionModule owner) NoPkgQual
      case found of
        Found location _ | ml_hi_file location == sessionHiPath liveRoot owner -> pure ()
        _ -> fail "future request did not resolve the published interface root"
    forM_ owners $ \owner -> do
      (_, globals, _) <- injectSessionIfaceWithBindings liveRoot owner initial
      global <- case globals of
        [actual] -> pure actual
        _ -> fail "published retry lost its unique actual global"
      let expected = if owner == firstOwner then "minus" else "answer"
      original <- case [typedCaptureIdentifier capture
            | item <- pendingSegmentItems retryTyped, capture <- typedItemCaptures item
            , occurrence (typedCaptureIdentifier capture) == expected] of
        [actual] -> pure actual
        _ -> fail "published retry lost its unique original capture"
      unless (eqType (idType original) (idType global))
        (fail "published retry changed its original capture type")

-- Distinct native binders can have the same OccName before GHC tidies an
-- interface. Exercise both one telescope and a nested scope through the real
-- binary writer and decoder; presentation text cannot distinguish this fault.
verifySigmaTransport :: HscEnv -> FilePath -> IO ()
verifySigmaTransport initial root = do
  supply <- mkSplitUniqSupply 't'
  let (firstUnique, remaining) = takeUniqFromSupply supply
      (secondUnique, _) = takeUniqFromSupply remaining
      variable unique = mkTyVar (mkInternalName unique (mkTyVarOcc "a") noSrcSpan) liftedTypeKind
      first = variable firstUnique
      second = variable secondUnique
      firstType = mkTyVarTy first
      secondType = mkTyVarTy second
      constantType = mkInfForAllTys [first, second]
        (mkVisFunTyMany firstType (mkVisFunTyMany secondType firstType))
      nestedType = mkInfForAllTy first (mkVisFunTyMany firstType
        (mkInfForAllTy second (mkVisFunTyMany secondType firstType)))
  unless (first /= second && nameOccName (varName first) == nameOccName (varName second))
    (fail "sigma transport prerequisite did not retain distinct colliding binders")
  forM_ (zip [681, 682] [constantType, nestedType]) $ \(generation, original) -> do
    let owner = SessionModule ValMod (Generation generation)
        binding = mkVarOcc "retainedSigma"
    iface <- mkThinSessionIface initial owner [(binding, original)]
    writeSessionIface initial root owner iface
    (_, decoded, _) <- injectSessionIfaceWithBindings root owner initial
    case decoded of
      [global] -> unless (eqType original (idType global))
        (fail "thin session interface collapsed scoped forall binders")
      _ -> fail "sigma session interface lost its unique actual binder"
    signature <- captureCheckedSignature initial "retained-sigma" original
    (resolved, _) <- resolveCheckedSignature initial signature
    unless (eqType original resolved)
      (fail "checked signature collapsed scoped forall binders")

-- Finalization captures every output once. Item visibility is carried by the
-- prepared capture selections rather than by rewriting finalized receipts.
verifyOutputProjection :: FilePath -> PreparedTypedSegmentBindings -> IO ()
verifyOutputProjection root prepared = do
  let environment = typedSegmentSessionEnvironment prepared
      snapshots = typedSegmentSessionInterfaces prepared
      evidence = DependencyEvidence False False [] [] [] []
      capture originals directory = do
        createDirectoryIfMissing True directory
        captureFinalizedModuleArtifacts originals environment
          Map.empty Map.empty evidence directory
      keys = map interfaceOwner
  unless (length snapshots == 2 && null (typedSegmentSessionInterfacesThrough (-1) prepared))
    (fail "capture selection requires two actual captures and an empty initial prefix")
  originals <- newOriginalInterfaceArtifactsWithSessionOutputs environment Map.empty [] [] snapshots
    (root </> "complete-output-evidence")
  full <- capture originals (root </> "complete-output-evidence")
  unless (map fst (finalizedValueInterfaceSeals full) == keys snapshots)
    (fail "complete original output census differs from actual batch")
  unless (keys (typedSegmentSessionInterfacesThrough 0 prepared) == keys (take 1 snapshots)
      && keys (typedSegmentSessionInterfacesThrough 1 prepared) == keys snapshots)
    (fail "prepared capture selection lost the current/prior generation boundary")
  imported <- newOriginalInterfaceArtifactsWithSessionOutputs environment Map.empty []
    (take 1 snapshots) (drop 1 snapshots) (root </> "imported-output-evidence")
  mixed <- capture imported (root </> "imported-output-evidence")
  unless (finalizedInterfaceSeals mixed == finalizedInterfaceSeals full)
    (fail "changing exact input/output roles changed immutable interface facts")

verifyBatch :: [SessionModule] -> PendingTypedSegment -> PreparedTypedSegmentBindings -> IO ()
verifyBatch owners typed prepared = do
  let captures = concatMap typedItemCaptures (pendingSegmentItems typed)
      pairs = typedSegmentSessionGlobals prepared
      bindings = concatMap snd (typedSegmentSessionBinders prepared)
      dependencies = typedSegmentSessionRetainedGlobals prepared
      expectedDependencies = [(global,generation)
        | (SessionModule _ (Generation generation),(_,global)) <- zip owners pairs]
  unless (dependencies == expectedDependencies)
    (fail "native capture dependency generation differs from its exact hydrated global")
  unless (length captures == 2 && length pairs == 2 && length bindings == 2
    && length (typedSegmentSessionInterfaces prepared) == 2)
    (fail "Session positive has an incomplete or empty capture inventory")
  unless (map typedCaptureFixity captures == [Just (Fixity 4 InfixR), Nothing])
    (fail "compiler-issued captures lost the actual local fixity")
  forM_ (zip (zip3 owners captures pairs) (typedSegmentSessionInterfaces prepared)) $ \((owner, capture, (original, global)), snapshot) -> do
    unless (original == typedCaptureIdentifier capture && occurrence original == occurrence global
      && eqType (idType original) (idType global))
      (fail "hydrated global differs from its original compiler capture")
    hmi <- maybe (fail "hydrated global has no actual HPT owner") pure
      (lookupHpt (hsc_HPT (typedSegmentSessionEnvironment prepared)) (renderSessionModule owner))
    unless (nameModule_maybe (idName global) == Just (fst (capturedSessionInterface snapshot)))
      (fail "hydrated global belongs to a different module")
    case mi_extra_decls (hm_iface hmi) of
      Nothing -> pure ()
      _ -> fail "thin value interface acquired original executable Core"
  unless (map bbTier bindings == [RetainOpaque, ForceData])
    (fail "typed captures changed the actual scalar/closure publication policy")
  unless (map bbVarId bindings == map (stableVarId . idName . snd) pairs)
    (fail "published binder identity is not the decoded global identity")
  assertFixities (typedSegmentSessionEnvironment prepared) owners

assertFixities :: HscEnv -> [SessionModule] -> IO ()
assertFixities env owners = forM_ (zip owners [[(mkVarOcc "minus", Fixity 4 InfixR)], []]) $ \(owner, expected) -> do
  hmi <- maybe (fail "fixity owner is missing from the hydrated HPT") pure
    (lookupHpt (hsc_HPT env) (renderSessionModule owner))
  unless (mi_fixities (hm_iface hmi) == expected) (fail "hydrated interface changed its actual fixities")

assertNoStagingFinder :: HscEnv -> FilePath -> SessionModule -> IO ()
assertNoStagingFinder env staging owner = do
  found <- findImportedModule env (renderSessionModule owner) NoPkgQual
  case found of
    Found location _ | ml_hi_file location == sessionHiPath staging owner ->
      fail "private staging location leaked into the finder"
    _ -> pure ()

copySnapshots :: FilePath -> [SessionModule] -> [CapturedSessionInterface] -> IO ()
copySnapshots root owners snapshots = do
  unless (length owners == length snapshots && not (null owners))
    (fail "fault input snapshot inventory is incomplete")
  forM_ (zip owners snapshots) $ \(owner, snapshot) -> do
    unless (renderSessionModule owner == moduleName (fst (capturedSessionInterface snapshot)))
      (fail "fault snapshot belongs to a different session owner")
    let path = sessionHiPath root owner
    createDirectoryIfMissing True (takeDirectory path)
    BS.writeFile path (snd (capturedSessionInterface snapshot))
    case capturedSessionInterfaceEvidence snapshot of
      Just (packages, requirements, _) -> do
        BS.writeFile (path ++ ".packages") packages
        BS.writeFile (path ++ ".requirements") requirements
      Nothing -> fail "real Session snapshot lacks complete evidence"

assertAbsent :: FilePath -> [SessionModule] -> IO ()
assertAbsent root owners = forM_ owners $ \owner -> forM_ (bindingPaths root owner) $ \path -> do
  exists <- doesPathExist path
  unless (not exists) (fail "refusal or cancellation left a partial generation output")

bindingPaths :: FilePath -> SessionModule -> [FilePath]
bindingPaths root owner = let path = sessionHiPath root owner
  in [path, path ++ ".packages", path ++ ".requirements"]

occurrence :: Id -> String
occurrence = occNameString . nameOccName . idName

temporary :: IO FilePath
temporary = do
  parent <- getTemporaryDirectory
  (path, handle) <- openTempFile parent "tidepool-typed-session"
  hClose handle
  removeFile path
  createDirectory path
  pure path
