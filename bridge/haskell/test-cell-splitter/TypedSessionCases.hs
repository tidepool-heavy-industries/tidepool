module TypedSessionCases (typedSessionHydrationPublicationChecks) where

import Control.Exception
  ( AsyncException(ThreadKilled), Exception, IOException, bracket, throwIO, try )
import Control.Monad (forM_, unless, void)
import Data.IORef (newIORef, readIORef, writeIORef)
import qualified Data.ByteString as BS
import GHC (getSessionDynFlags, runGhc)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.Type (liftedTypeKind, mkInfForAllTy, mkInfForAllTys, mkTyVarTy, mkVisFunTyMany)
import GHC.Driver.Env (HscEnv, hsc_HPT)
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
import GHC.Unit.Module (moduleName)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Module.ModIface (mi_fixities, mi_extra_decls)
import System.Directory
  ( createDirectory, createDirectoryIfMissing, doesPathExist
  , getTemporaryDirectory, pathIsSymbolicLink, removeDirectoryRecursive, removeFile )
import System.FilePath ((</>), takeDirectory)
import System.IO (openTempFile, hClose)
import System.IO.Error (isAlreadyExistsError)
import System.Posix.Files (createSymbolicLink, readSymbolicLink)
import Tidepool.Binders
  ( BoundBinder(..), ValueTier(..), CellSplitError(..)
  , analyzeCellWithFlags, analyzeOrderedCellWithFlags
  , prepareTypedSegmentSource, preparedTypedSegmentPlan
  , preparedTypedSegmentSource, preparedTypedSegmentOperations )
import Tidepool.ExtractUtil (getLibdir)
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
  ( TypedSegment
  , typedSegmentItems, typedItemCaptures, typedCaptureIdentifier
  , typedCaptureType, typedCaptureFixity )

data SessionBoundaryFailure = HydrationCompletionRefused | PublicationCompletionRefused
  deriving (Eq, Show)
instance Exception SessionBoundaryFailure

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
    answerCapture <- case [capture | item <- typedSegmentItems typed
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
            | item <- typedSegmentItems retryTyped, capture <- typedItemCaptures item
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

verifyBatch :: [SessionModule] -> TypedSegment -> PreparedTypedSegmentBindings -> IO ()
verifyBatch owners typed prepared = do
  let captures = concatMap typedItemCaptures (typedSegmentItems typed)
      pairs = typedSegmentSessionGlobals prepared
      bindings = concatMap snd (typedSegmentSessionBinders prepared)
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
