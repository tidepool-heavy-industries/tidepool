{-# LANGUAGE ScopedTypeVariables #-}
module CompilerRequestTest (nativePackageOutputChecks, nativeOutputProvenanceChecks, completedRequestHistories, completedRequestCancellation, completedRequestRelease, checkedPublicModuleHistories, worldRequestBoundaries, worldMaterializationBoundaries) where

import Tidepool.RequestCompletion
  ( finishCompletion, captureCompletion, completionResult, completionFailures
  , BodyOutcome(..), CleanupStage(..), cleanupStage, cleanupException, withCompletionRelease )

import Control.Exception (AsyncException(ThreadKilled), SomeException, SomeAsyncException, IOException, fromException, throw, throwIO, try)
import Control.Concurrent (forkFinally, killThread, newEmptyMVar, putMVar, readMVar)
import Data.IORef (newIORef, readIORef, writeIORef)
import Data.List (isInfixOf)
import qualified Data.Text as Text
import qualified Data.Text.Encoding as TextEncoding
import System.Timeout (timeout)
import System.IO (stderr)
import Control.Monad ((>=>), forM, forM_, unless, void)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm)
import qualified Data.ByteString.Lazy as BL
import Control.Monad.IO.Class (liftIO)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.ByteString as BS
import GHC (findModule, getModuleInfo, modInfoExports, mkModuleName, getSessionDynFlags)
import GHC.Driver.Backend (backendGeneratesCode)
import GHC.Driver.Session (backend, gopt, GeneralFlag(Opt_ByteCode), parseDynamicFilePragma)
import GHC.Types.SrcLoc (noLoc)
import GHC.Types.Name (getOccString)
import Tidepool.WorkerDiagnostics (sourceFailureDiagnostics)
import GHC.Tc.Types (tcg_mod)
import GHC.Unit.Module (moduleName, moduleNameString)
import SourceBootFixtureSupport (hasIntResultLiteral, withScratch, captureDiagnostics
  , withRootedCompilerEnvironment, withQuietStdout)
import System.Directory (copyFile, createDirectory, doesDirectoryExist, doesFileExist, getFileSize, getPermissions, setPermissions, listDirectory, removeFile, Permissions(..))
import System.FilePath ((</>), takeFileName, takeDirectory)
import Test.QuickCheck
import Tidepool.CompilerProducts
  ( prepareCompilerProjectionContext, newPreparedOriginalInterfaceArtifacts, preparedProductInventory, writeCertifiedProductsKeepingWithOriginals )
import Tidepool.CompilerRequest
import Tidepool.ExecutionProjection (preparedModuleProductOutcomes, projectOriginalHomeModuleProducts)
import Tidepool.ExtractRequest
import Tidepool.GhcPipeline
import Tidepool.ExecutionSchema (WireProgram(..), SymbolIdentity(..), Group(..), TopBinding(..))
import Tidepool.DependencyEvidence (DependencyEvidence(..), DependencyModule(..))
import Tidepool.PackageWitness (requireObservationPackageEnvironment)
import Tidepool.ExactScope (decodeWorldSources)
import Tidepool.DiagJson (InputRejection(..))
import Tidepool.CertifiedProducts (captureSelectedPackageWitnesses)
import Tidepool.ArtifactBytes (artifactSha256, artifactBytes, captureArtifactBytes)

worldRequestBoundaries :: IO ()
worldRequestBoundaries = do
  worldAcquisitionDiagnostics
  requireObservationPackageEnvironment
  let root = replicate 64 'a'
      foreignReference = replicate 64 '1' ++ ".7"
      resident = ExactInputsRequest root (ResidentExactInputs foreignReference)
      complete = ExactInputsRequest root (CompleteExactInputs "/unavailable/world-inputs.cbor")
      question = WorldQueryRequest root foreignReference (WorldPublicModuleQuery (WorldPublicInfo
        (StructuredInspection (StructuredPublicModule "Data.List") StructuredValueName "map"
          (InspectionProvenance 1 "display-only")))) "/unavailable/world-answer.bin"
      body = BS.pack [136,1,88,32] <> BS.replicate 32 170 <> BS.pack [0,105] <> "Data.List"
        <> BS.pack [132,1,99] <> "map" <> BS.pack [1,108] <> "display-only"
        <> BS.pack [130,104] <> "rejected" <> BS.pack [1,246,64]
      expected = "TPWQ0002" <> BS.pack [80,0,0,0] <> body
  args <- case workerRequestFromArgv (workerArgv [WorldQuery question]) of
    Right (Just request) -> pure request
    other -> fail ("invalid world query fixture: " ++ show other)
  (lateEnsure,lateQuery) <- withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \scope -> do
    forM_ [resident,complete] $ \request -> do
      outcome <- scopedEnsureWorld scope request
      unless (outcome == ExactInputsRejected UnsupportedOperation)
        (fail "observation boot acquired a world or read complete inputs")
    (withBoundCompilerRequest (const (pure ())) scope args >=> finishCompletion) $ \request -> do
      actual <- runBoundWorldQuery request
      unless (actual == expected) (fail "missing scope hold lost its typed unavailable response")
      generic <- try (runBoundGhc request (pure ())) :: IO (Either SomeException ())
      case generic of
        Left failure | Just CheckedPurposeMismatch <- fromException failure -> pure ()
        Left failure -> throwIO failure
        Right _ -> fail "held-world request entered the generic GHC operation path"
      pure (void (scopedEnsureWorld scope resident),void (runBoundWorldQuery request))
  requirePhase CompilerTransactionReleased lateEnsure
  try lateQuery >>= requireCancellation
  let recipe = WorldCellRecipe "(this must not parse" "module CellCheck where" []
      cellPlan = WorldCellPlanRequest root foreignReference recipe Nothing "/unavailable/cell-plan.cbor"
      cellPrepare = WorldCellPrepareRequest root foreignReference recipe Nothing "untrusted-receipt" (BS.replicate 32 1) 17
        [] "/unavailable/cell-operation" "/unavailable/cell-prepared.cbor"
      decode fields = case workerRequestFromArgv (workerArgv fields) of
        Right (Just request) -> pure request
        other -> fail ("invalid held cell fixture: " ++ show other)
  let decodeSlots slots = workerRequestFromArgv (workerArgv
        [WorldCellPrepare cellPrepare {worldCellPrepareSlots=slots}])
  case decodeSlots [(0,PlannedDeclaration 1),(1,PlannedBind 1)] of
    Right (Just _) -> pure ()
    other -> fail ("held request conflated Lib1 and Val1 reservations: " ++ show other)
  forM_ [[(0,PlannedBind 1),(1,PlannedExpression 1 "observation")]
    ,[(0,PlannedPrologue 1),(1,PlannedDeclaration 1)]] $ \slots ->
      case decodeSlots slots of
        Left _ -> pure ()
        other -> fail ("held request admitted duplicate module reservations: " ++ show other)
  planArgs <- decode [WorldCellPlan cellPlan]
  prepareArgs <- decode [WorldCellPrepare cellPrepare]
  (latePlan,latePrepare) <- withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \scope -> do
    latePlan <- (withBoundCompilerRequest (const (pure ())) scope planArgs >=> finishCompletion) $ \request -> do
      absent <- try (runBoundWorldCellPlan request) :: IO (Either WorldCellRefusal BS.ByteString)
      unless (either (== WorldCellUnavailable) (const False) absent)
        (fail "unheld plan parsed source or obtained a compiler world")
      pure (void (runBoundWorldCellPlan request))
    latePrepare <- (withBoundCompilerRequest (const (pure ())) scope prepareArgs >=> finishCompletion) $ \request -> do
      absent <- boundWorldCellPreparation request
      unless (either (== WorldCellUnavailable) (const False) absent)
        (fail "unheld prepare parsed source or allocated compiler generation state")
      compiled <- try (compileBoundRequest request CheckedEnvironment Set.empty GeneralCompile Nothing "/unavailable/target.hs")
        :: IO (Either WorldCellRefusal CheckedEnvironmentResult)
      unless (either (== WorldCellUnavailable) (const False) compiled)
        (fail "world compiler bypassed its actual hold/admission owner")
      conflict <- try (compileBoundRequest request CheckedEnvironment Set.empty
        (ParsedImportSelection [] (ParsedImportSelection [] GeneralCompile)) Nothing "/unavailable/target.hs")
        :: IO (Either CompilePurposeConflict CheckedEnvironmentResult)
      unless (either (== DuplicateCompilePurposeSetting "ParsedImportSelection") (const False) conflict)
        (fail "conflicting compile purpose was not refused before held-world work")
      pure (void (boundWorldCellPreparation request))
    pure (latePlan,latePrepare)
  try latePlan >>= requireCancellation
  try latePrepare >>= requireCancellation

-- Exercise the production human diagnostic emitter without manufacturing a
-- rooted package/world. Actual Complete dispatch still requires the HOST probe.
worldAcquisitionDiagnostics :: IO ()
worldAcquisitionDiagnostics = do
  let reason = "read/decode WorldInputsCorrupt"
      manifest = "/captured/\x03bb/world.cbor"
      detail = "fixture decode failure: \x03bb"
      emittedBytes = BS.length . TextEncoding.encodeUtf8 . Text.pack
  (_,ordinary) <- captureDiagnostics (emitWorldAcquisitionRejection reason manifest detail)
  unless (all (`isInfixOf` ordinary) [reason,manifest,detail])
    (fail "world acquisition diagnostic lost its typed read cause/path/detail")
  -- A valid UTF-8 stream may cross the existing capture byte boundary.
  -- Decode its complete prefix, without treating a split final code point as
  -- malformed data or replacing it with a different character.
  let captureLimit = 4 * 1024 * 1024
      truncatedMarker = "\n[compiler diagnostics truncated at 4 MiB]\n"
  (_,truncated) <- captureDiagnostics $ BS.hPut stderr
    (BS.replicate (captureLimit - 1) 97 <> TextEncoding.encodeUtf8 (Text.pack "\x03bb"))
  unless (truncated == replicate (captureLimit - 1) 'a' ++ truncatedMarker)
    (fail "bounded compiler diagnostics rejected or changed a valid UTF-8 prefix")
  limited <- timeout 5000000 $ captureDiagnostics $
    emitWorldAcquisitionRejection reason (repeat '\x1f30a') (repeat '\n')
  case limited of
    Just (_,output) | reason `isInfixOf` output
      , "diagnostic field exceeds 4096 UTF-8 bytes" `isInfixOf` output
      , emittedBytes output < 74 * 1024 -> pure ()
    _ -> fail "world acquisition diagnostic did not bound lazy UTF-8 fields"
  (_,escaped) <- captureDiagnostics $
    emitWorldAcquisitionRejection reason (replicate 4096 '\0') (replicate 4096 '\n')
  unless (emittedBytes escaped < 74 * 1024 && "\\u0000" `isInfixOf` escaped)
    (fail "world acquisition diagnostic JSON expansion exceeded its bounded allowance")
  -- A lazy formatting IOException reaches the actual emitter's exception
  -- boundary. This is not a physical stderr/capture failure simulation.
  (refusal,_) <- captureDiagnostics $ do
    emitWorldAcquisitionRejection reason manifest
      (throw (userError "injected world diagnostic formatting failure"))
    pure (ExactInputsRejected AcquisitionRejected)
  unless (refusal == ExactInputsRejected AcquisitionRejected)
    (fail "diagnostic failure replaced the original acquisition rejection")
  cancelled <- try (captureDiagnostics (emitWorldAcquisitionRejection reason manifest
    (throw ThreadKilled))) :: IO (Either SomeException ((),String))
  case cancelled of
    Left failure | fromException failure == Just ThreadKilled -> pure ()
    Left failure -> throwIO failure
    Right _ -> fail "world acquisition diagnostic swallowed cancellation"

-- These are raw closed-source data/I/O controls, not rooted world issuers.
-- They enter the same materialization owner used before world publication.
worldMaterializationBoundaries :: IO ()
worldMaterializationBoundaries = do
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\nanswer = 1\n")]
    materialized <- materializeWorldSources parent sources Set.empty pure
    child <- onlyChild parent
    let directory = parent </> child
    -- Three direct realized owners share exactly one physical source tree.
    retainWorldMaterialization materialized
    retainWorldMaterialization materialized
    owners <- worldMaterializationOwners materialized
    unless (owners == Just 3) (fail "world fork did not retain three direct backing owners")
    forM_ [2,1] $ \remaining -> do
      removed <- releaseWorldMaterialization materialized
      alive <- doesDirectoryExist directory
      owners <- worldMaterializationOwners materialized
      unless (not removed && alive && owners == Just remaining)
        (fail "dropping a predecessor removed surviving successor backing")
      measured <- measureBacking directory
      unless (measured == worldMaterializationCharge materialized)
        (fail "shared source backing changed its once-issued physical charge")
    removed <- releaseWorldMaterialization materialized
    alive <- doesDirectoryExist directory
    owners <- worldMaterializationOwners materialized
    unless (removed && not alive && owners == Just 0)
      (fail "last realized world failed to release its physical backing")
    repeated <- try (releaseWorldMaterialization materialized) :: IO (Either SomeException Bool)
    unless (either (const True) (const False) repeated)
      (fail "released backing accepted a duplicate release")
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    first <- materializeWorldSources parent sources Set.empty pure
    firstChild <- onlyChild parent
    second <- materializeWorldSources parent sources Set.empty pure
    children <- listDirectory parent
    secondChild <- case filter (/= firstChild) children of
      [child] -> pure child
      _ -> fail "independent cold acquisitions did not keep distinct physical backing"
    firstOwners <- worldMaterializationOwners first
    secondOwners <- worldMaterializationOwners second
    firstCharge <- measureBacking (parent </> firstChild)
    secondCharge <- measureBacking (parent </> secondChild)
    unless (firstOwners == Just 1 && secondOwners == Just 1
        && firstCharge == worldMaterializationCharge first
        && secondCharge == worldMaterializationCharge second)
      (fail "equal source bytes substituted sharing for actual physical acquisitions")
    _ <- releaseWorldMaterialization first
    remaining <- doesDirectoryExist (parent </> secondChild)
    unless remaining (fail "independent acquisition lost backing when its equal peer retired")
    _ <- releaseWorldMaterialization second
    requireEmpty parent
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    materialized <- materializeWorldSources parent sources Set.empty pure
    child <- onlyChild parent
    let directory = parent </> child
        root = directory </> "root-0"
    permissions <- getPermissions root
    setPermissions root (permissions {readable=False,writable=False,searchable=False})
    failed <- try (releaseWorldMaterialization materialized) :: IO (Either SomeException Bool)
    setPermissions root permissions
    unless (either (const True) (const False) failed)
      (fail "unreadable backing unexpectedly reported successful cleanup")
    owners <- worldMaterializationOwners materialized
    unless (owners == Nothing) (fail "cleanup failure reported settled backing ownership")
    retained <- try (retainWorldMaterialization materialized) :: IO (Either SomeException ())
    unless (either (const True) (const False) retained)
      (fail "quarantined backing accepted a new successor")
    retried <- try (releaseWorldMaterialization materialized) :: IO (Either SomeException Bool)
    unless (either (const True) (const False) retried)
      (fail "quarantined backing manufactured a successful release")
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\nanswer = 1\n")
      ,("Nested/B.hs","module Nested.B where\n")]
    charged <- materializeWorldSources parent sources Set.empty (pure . worldMaterializationCharge)
    child <- onlyChild parent
    bytes <- BS.readFile (parent </> child </> "root-0" </> "A.hs")
    unless (bytes == "module A where\nanswer = 1\n")
      (fail "successful world materialization removed or changed its issued files")
    -- Independent cold-test filesystem oracle, never a production hit scan.
    measured <- measureBacking (parent </> child)
    unless (charged == measured) (fail ("world backing issuance charge differs: " ++ show (charged,measured)))
  withScratch $ \parent -> do
    -- A file/parent collision deterministically fails after the first copy.
    sources <- sourceFixture [("A","copied"),("A/B.hs","module A.B where\n")]
    entered <- newIORef False
    outcome <- try (materializeWorldSources parent sources Set.empty
      (const (writeIORef entered True))) :: IO (Either SomeException ())
    case outcome of
      Left failure | Just (_ :: IOException) <- fromException failure -> pure ()
      Left failure -> throwIO failure
      Right () -> fail "world materializer accepted a file/directory collision"
    published <- readIORef entered
    unless (not published) (fail "failed source copy entered world publication")
    requireEmpty parent
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    outcome <- try (materializeWorldSources parent sources Set.empty
      (const (throwIO policy))) :: IO (Either SomeException ())
    requirePolicy outcome
    requireEmpty parent
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    ready <- newEmptyMVar
    blocked <- newEmptyMVar
    done <- newEmptyMVar
    let action = materializeWorldSources parent sources Set.empty $ \_ -> do
          putMVar ready ()
          readMVar blocked
        retire thread = bounded "world materialization child retirement"
          (killThread thread >> readMVar done) >> pure ()
    completed <- withCompletionRelease ExecutorStop
      (forkFinally action (putMVar done)) retire $ \thread -> captureCompletion $ do
        bounded "world materialization publication barrier" (readMVar ready)
        bounded "world materialization cancellation" (killThread thread)
        result <- bounded "cancelled world materialization completion" (readMVar done)
        requireCancellation result
        requireEmpty parent
    finishCompletion completed
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    recovery <- newIORef Nothing
    completed <- captureCompletion $ materializeWorldSources parent sources Set.empty $ \_ -> do
      child <- onlyChild parent
      let root = parent </> child </> "root-0"
      permissions <- getPermissions root
      writeIORef recovery (Just (root,permissions))
      setPermissions root (permissions {readable=False,writable=False,searchable=False})
      throwIO policy
    restore <- readIORef recovery
    forM_ restore $ \(root,permissions) -> setPermissions root permissions
    case completionResult completed of
      BodyFailed failure -> requirePolicy (Left failure)
      _ -> fail "failed world cleanup lost its original policy refusal"
    case completionFailures completed of
      [failure] | cleanupStage failure == CompilerScopeRestore
        , Just (_ :: IOException) <- fromException (cleanupException failure) -> pure ()
      _ -> fail "failed world cleanup lost its independent release evidence"
    _ <- onlyChild parent
    pure ()
  where
    policy = WorldModulePolicyFailure "fixture-source" WorldUncapturedDependency
    requirePolicy result = case result of
      Left failure | Just (WorldModulePolicyFailure path reason) <- fromException failure
        , path == "fixture-source", reason == WorldUncapturedDependency -> pure ()
      Left failure -> throwIO failure
      Right () -> fail "unissued world unexpectedly survived policy refusal"
    requireEmpty parent = do
      children <- listDirectory parent
      unless (null children) (fail ("unissued world directory survived failure: " ++ show children))
    onlyChild parent = do
      children <- listDirectory parent
      case children of
        [child] -> pure child
        _ -> fail ("expected one owned world directory, got " ++ show children)
    bounded label action = timeout 5000000 action >>= maybe (fail (label ++ " timed out")) pure
    measureBacking path = do
      directory <- doesDirectoryExist path
      if directory then do
        children <- listDirectory path
        charges <- mapM (measureBacking . (path </>)) children
        pure (sum (map fst charges),1 + sum (map snd charges))
      else do
        bytes <- getFileSize path
        pure (bytes,1)
    sourceFixture files = either fail pure (decodeWorldSources
      ("TPCSRC01" <> frame "fixture-generation" <> word32 1 <> word32 (length files)
        <> foldMap (\(path,body) -> frame path <> frame body) files))
    frame bytes = word32 (BS.length bytes) <> bytes
    word32 value = BS.pack [fromIntegral (value `div` (256 ^ power)) | power <- [0..3 :: Int]]

-- Actual module checking enters the same bound executor and pipeline as the
-- service query. Refusals must identify the transitive dependency, before
-- preprocessing/typechecking has an opportunity to execute its options.
checkedPublicModuleHistories :: IO ()
checkedPublicModuleHistories = do
  withRequestFixture $ \_ request work -> do
    runBoundGhc request $ do
      baseline <- getSessionDynFlags
      (effective,leftover,_) <- parseDynamicFilePragma baseline [noLoc "-fbyte-code"]
      liftIO $ unless (null leftover
          && not (backendGeneratesCode (backend baseline))
          && not (gopt Opt_ByteCode baseline)
          && not (backendGeneratesCode (backend effective))
          && gopt Opt_ByteCode effective)
        (fail "pinned GHC bytecode pragma no longer separates its generation flag from the checking backend")
    writeTarget work
    checked <- compileBoundRequest request CheckedPublicModule Set.empty GeneralCompile Nothing
      (work </> "BoundPreparedA.hs")
    unless (moduleNameString (moduleName (checkedPublicModuleIdentity checked)) == "BoundPreparedA"
        && Set.fromList (map dependencyModuleName (dependencyModules (checkedPublicModuleEvidence checked)))
          == Set.fromList ["BoundPreparedA","BoundPreparedB"])
      (fail "public module completion lost the actual target or dependency graph")
    -- Ordinary consumers still receive their target's checked environment.
    ordinary <- compileBoundRequest request CheckedEnvironment Set.empty GeneralCompile Nothing
      (work </> "BoundPreparedA.hs")
    unless (moduleNameString (moduleName (tcg_mod (crTargetTcGblEnv ordinary))) == "BoundPreparedA")
      (fail "ordinary checked consumer changed after public module completion")
  forM_ ["{-# LANGUAGE CPP #-}","{-# LANGUAGE TemplateHaskell #-}"] $ \pragma ->
    withRequestFixture $ \_ request work -> do
      writeTarget work
      prependDependency work pragma
      refused request (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" WorldUntrackedOptions
  forM_ [("-ddump-parsed -ddump-to-file -dumpdir ",WorldSideOutputs)
        ,("-fwrite-ide-info -hiedir ",WorldSideOutputs)
        ,("-hidir ",WorldOutputPolicyChanged)
        ,("-tmpdir ",WorldOutputPolicyChanged)
        ,("-ddump-file-prefix ",WorldOutputPolicyChanged)] $ \(flags,reason) ->
    withRequestFixture $ \_ request work -> do
      writeTarget work
      let ambient = work </> "ambient-outputs"
      createDirectory ambient
      prependDependency work ("{-# OPTIONS_GHC " ++ flags ++ show ambient ++ " #-}")
      refused request (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" reason
      outputs <- listDirectory ambient
      unless (null outputs) (fail ("world module admission wrote untracked outputs: " ++ show outputs))
  forM_ ["-fwrite-interface","-fwrite-if-simplified-core","-shared"] $ \flag ->
    withRequestFixture $ \_ request work -> do
      writeTarget work
      prependDependency work ("{-# OPTIONS_GHC " ++ flag ++ " #-}")
      refused request (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" WorldOutputPolicyChanged
  forM_ ["-fbyte-code","-fobject-code"] $ \backendFlag ->
    withRequestFixture $ \_ request work -> do
      writeTarget work
      let compiler = work </> "foreign-compiler.sh"
          marker = work </> "foreign-compiler-ran"
      writeFile compiler ("#!/bin/sh\necho ran > " ++ show marker ++ "\nexit 1\n")
      permissions <- getPermissions compiler
      setPermissions compiler (permissions {executable=True})
      writeFile (work </> "BoundPreparedB.hs") (unlines
        [ "{-# LANGUAGE CApiFFI #-}"
        , "{-# OPTIONS_GHC " ++ backendFlag ++ " -pgmc " ++ show compiler ++ " #-}"
        , "module BoundPreparedB where"
        , "import Foreign.C.Types (CInt(..))"
        , "foreign import capi unsafe \"uncaptured_world_header.h world_probe\" probe :: IO CInt"
        , "__result :: Int"
        , "__result = 97"
        ])
      refused request (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" WorldCodeGeneration
      ran <- doesFileExist marker
      unless (not ran) (fail "world module admission executed a transitive foreign compiler")
  withRequestFixture $ \_ request work -> do
    writeTarget work
    let preprocessor = work </> "preprocessor.sh"
        marker = work </> "preprocessor-ran"
    writeFile preprocessor ("#!/bin/sh\necho ran > " ++ show marker ++ "\nexit 1\n")
    permissions <- getPermissions preprocessor
    setPermissions preprocessor (permissions {executable=True})
    prependDependency work ("{-# OPTIONS_GHC -F -pgmF " ++ show preprocessor ++ " #-}")
    refused request (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" WorldUntrackedOptions
    ran <- doesFileExist marker
    unless (not ran) (fail "world module admission executed a transitive custom preprocessor")
  withRequestFixture $ \_ request work -> do
    let literate = work </> "BoundPreparedA.lhs"
    writeFile literate "> module BoundPreparedA where\n> answer :: Int\n> answer = 41\n"
    refused request literate "BoundPreparedA.lhs" WorldLiterateSource
  where
    writeTarget work = writeFile (work </> "BoundPreparedA.hs")
      "module BoundPreparedA where\nimport qualified BoundPreparedB\nanswer :: Int\nanswer = BoundPreparedB.__result\n"
    prependDependency work pragma = do
      let path = work </> "BoundPreparedB.hs"
      original <- readFile path
      -- Force the old text before reopening the same path for writing.
      length original `seq` writeFile path (pragma ++ "\n" ++ original)
    refused request path dependency reason = do
      outcome <- try (compileBoundRequest request CheckedPublicModule Set.empty GeneralCompile Nothing path)
        :: IO (Either SomeException CheckedPublicModuleResult)
      case outcome of
        Left failure
          | Just (WorldModulePolicyFailure actual actualReason) <- fromException failure
          , takeFileName actual == dependency, actualReason == reason -> pure ()
          | otherwise -> throwIO failure
        Right _ -> fail "unsupported world module source was checked"

requestFor :: FilePath -> IO WorkerRequest
requestFor work = case workerRequestFromArgv (workerArgv [Input (work </> "BoundPreparedA.hs"), Include work]) of
  Right (Just args) -> pure args
  other -> fail ("invalid bound compiler fixture request: " ++ show other)

withRequestFixture :: (CompilerScope -> BoundCompilerRequest -> FilePath -> IO a) -> IO a
withRequestFixture action = withScratch $ \work -> do
  forM_ ["BoundPreparedA.hs", "BoundPreparedB.hs", "BoundPreparedError.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  args <- requestFor work
  withRootedCompilerEnvironment work $ withQuietStdout $
    withResidentCompilerScopes [work] $ \runScope -> runScope (pure ()) $ \scope ->
      (withBoundCompilerRequest (const (pure ())) scope args >=> finishCompletion) $ \request -> action scope request work

compileA :: BoundCompilerRequest -> FilePath -> IO PreparedCompilation
compileA request work = compilePreparedRequest request PreparedStg Set.empty GeneralCompile Nothing
  (work </> "BoundPreparedA.hs")

-- The expected owner and Core literal are authored fixture facts, not values
-- recovered from the retained cache. Recompute original products without the
-- completion observer/worklist as a separate oracle for delayed consumption.
projectA :: String -> PreparedCompilation -> IO ()
projectA target completion = do
  let prepared = preparedCompilationResult completion
      result = pprPipelineResult prepared
      env = prHscEnv result
      owner = tcg_mod (prTargetTcGblEnv result)
      interfaces = pprProductInterfaces prepared
      modules = pprModules prepared
  unless (moduleNameString (moduleName owner) == "BoundPreparedA"
      && hasIntResultLiteral 41 (prBinds result)) $
    fail "completed preparation lost its independently specified owner or Core value"
  context <- prepareCompilerProjectionContext prepared Map.empty owner target [] Nothing
  (_,actual) <- prepareCompilationOriginals completion Nothing context Set.empty
  let reference = projectOriginalHomeModuleProducts env interfaces context Set.empty modules
  unless (preparedModuleProductOutcomes (preparedProductInventory actual)
      == preparedModuleProductOutcomes reference) $
    fail "delayed completion products differ from uncached projection of its actual compiler inputs"

completedRequestHistories :: IO ()
completedRequestHistories = do
  runHistory [0,1,2,3,4]
  -- Each generated case and shrink recreates its owning universe, executor and
  -- first completion. A minimized history has no hidden earlier case prefix.
  result <- quickCheckWithResult stdArgs {maxSuccess=8,maxSize=8} $
    forAllShrink (resize 8 (listOf (chooseInt (0,4)))) shrink $ \history ->
      ioProperty (runHistory history >> pure True)
  unless (isSuccess result) (fail "completed compiler histories failed")

runHistory :: [Int] -> IO ()
runHistory history = withRequestFixture $ \_ request work -> do
  first <- compileA request work
  projectA "__result" first
  let step :: Int -> IO ()
      step choice = case choice of
        0 -> do
          second <- compilePreparedRequest request PreparedStg Set.empty GeneralCompile Nothing
            (work </> "BoundPreparedB.hs")
          unless (hasIntResultLiteral 97 (prBinds (pprPipelineResult (preparedCompilationResult second)))) $
            fail "B did not exercise an independent successful compiler result"
        1 -> do
          refused <- try (compileBoundRequest request CheckedEnvironment Set.empty GeneralCompile Nothing
            (work </> "BoundPreparedError.hs")) :: IO (Either SomeException CheckedEnvironmentResult)
          case refused of
            Left failure | Just (_ :: SomeAsyncException) <- fromException failure -> throwIO failure
            Left failure | Just _ <- sourceFailureDiagnostics failure -> pure ()
            Left failure -> throwIO failure
            Right _ -> fail "independent invalid Haskell fixture unexpectedly compiled"
        2 -> runBoundGhc request $ do
          package <- findModule (mkModuleName "Data.Maybe") Nothing
          info <- getModuleInfo package
          unless (maybe False (any ((== "fromMaybe") . getOccString) . modInfoExports) info) $
            liftIO (fail "package-only helper did not demand the actual Data.Maybe interface")
        3 -> projectA "__result" first
        _ -> projectA "alternate" first
  mapM_ step history
  projectA "__result" first

completedRequestCancellation :: IO ()
completedRequestCancellation = withRequestFixture $ \scope request work -> do
  first <- compileA request work
  originals <- newPreparedOriginalInterfaceArtifacts (preparedCompilationResult first) work
  cancelled <- try (runBoundGhc request (liftIO (throwIO ThreadKilled))) :: IO (Either SomeException ())
  requireCancellation cancelled
  unless (hasIntResultLiteral 41 (prBinds (pprPipelineResult (preparedCompilationResult first)))) $
    fail "cancellation changed previously completed compiler facts"
  requirePhase CompilerTransactionFailed (prepareArtifacts originals work first [] [] Map.empty [])
  -- The existing scope guard, rather than a second request lifecycle, refuses
  -- all later compiler effects too.
  requirePhase CompilerTransactionFailed (scopedRunGhc scope (pure ()))

completedRequestRelease :: IO ()
completedRequestRelease = withScratch $ \work -> do
  copyFile "test-source-boot/fixtures/BoundPreparedA.hs" (work </> "BoundPreparedA.hs")
  args <- requestFor work
  withResidentCompilerScopes [work] $ \runScope -> runScope (pure ()) $ \scope -> do
    completed <- (withBoundCompilerRequest (const (pure ())) scope args >=> finishCompletion) $ \request -> do
      first <- compileA request work
      projectA "__result" first
      originals <- newPreparedOriginalInterfaceArtifacts (preparedCompilationResult first) work
      pure (first,originals)
    -- The original GHC scope is Ready, but this executor is closed. Check an
    -- otherwise empty/cached operation, without relying on a task submission.
    let (first,originals) = completed
    refused <- try (void (prepareArtifacts originals work first [] [] Map.empty []))
      :: IO (Either SomeException ())
    requireCancellation refused
    -- A later live allowance on this same GHC scope cannot substitute for the
    -- completed result's closed issuing allowance, even before pending lookup.
    (withBoundCompilerRequest (const (pure ())) scope args >=> finishCompletion) $ \later -> do
      stolen <- try (commitPreparedWorldDependencies later first) :: IO (Either SomeException ())
      requireCancellation stolen

requireCancellation :: Either SomeException () -> IO ()
requireCancellation result = case result of
  Left failure | Just ThreadKilled <- fromException failure -> pure ()
  Left failure -> throwIO failure
  Right () -> fail "closed or cancelled owner accepted effectful preparation"

requirePhase :: CompilerTransactionFailure -> IO a -> IO ()
requirePhase expected action = do
  result <- try (void action) :: IO (Either SomeException ())
  case result of
    Left failure | Just actual <- fromException failure, actual == expected -> pure ()
    Left failure -> throwIO failure
    Right () -> fail ("compiler preparation accepted invalid phase: " ++ show expected)


nativeOutputProvenanceChecks :: IO ()
nativeOutputProvenanceChecks = withScratch $ \work -> do
  requireObservationPackageEnvironment
  copyFile "test-source-boot/fixtures/BoundPreparedA.hs" (work </> "BoundPreparedA.hs")
  native <- requestFor work
  portable <- case workerRequestFromArgv (workerArgv
      [Input (work </> "BoundPreparedA.hs"),Include work,Target "__result",OutputDir work,PortableOutput]) of
    Right (Just request) -> pure request
    other -> fail ("invalid explicit portable fixture: " ++ show other)
  withResidentCompilerScopes [work] $ \runScope -> runScope (pure ()) $ \scope -> do
    refused <- try ((withBoundCompilerRequest (const (pure ())) scope native >=> finishCompletion) $ \request ->
      compilePreparedRequest request PreparedStg Set.empty GeneralCompile Nothing (work </> "missing.hs"))
      :: IO (Either NativeProductConfigurationUnavailable PreparedCompilation)
    case refused of
      Left NativeProductConfigurationUnavailable -> pure ()
      Right _ -> fail "observation configuration issued a native compilation"
    (withBoundCompilerRequest (const (pure ())) scope portable >=> finishCompletion) $ \request -> do
      completion <- compilePreparedRequest request PreparedStg Set.empty GeneralCompile Nothing
        (work </> "BoundPreparedA.hs")
      let prepared = preparedCompilationResult completion
      unless (hasIntResultLiteral 41 (prBinds (pprPipelineResult prepared)))
        (fail "explicit portable compilation lost its actual source result")
      interfaces <- newPreparedOriginalInterfaceArtifacts prepared work
      output <- prepareArtifactsWithIntent interfaces work completion ["__result"] [] Map.empty []
      case output of
        PortableOutputs [artifact] -> unless (not (BS.null (paBytes artifact)))
          (fail "explicit portable compilation emitted no code")
        _ -> fail "explicit portable intent selected native products"

-- Finite installers and World stages consume this same native output owner.
-- A real list operation requires a separately prepared package implementation.
nativePackageOutputChecks :: IO ()
nativePackageOutputChecks = forM_ cases $ \(name,sourceLines,names,wantsCode) -> withScratch $ \work -> do
  let source = work </> name ++ ".hs"
  writeFile source (unlines sourceLines)
  args <- case workerRequestFromArgv (workerArgv ([Input source,Include work,OutputDir work] ++ map Target names)) of
    Right (Just request) -> pure request
    other -> fail ("invalid native package fixture: " ++ show other)
  withRootedCompilerEnvironment work $ withQuietStdout $
    withResidentCompilerScopes [work] $ \runScope -> runScope (pure ()) $ \scope ->
      (withBoundCompilerRequest (const (pure ())) scope args >=> finishCompletion) $ \request -> do
        completion <- compilePreparedRequest request (PreparedProducts Nothing) Set.empty GeneralCompile Nothing source
        let prepared = preparedCompilationResult completion
        interfaces <- newPreparedOriginalInterfaceArtifacts prepared work
        output <- prepareArtifactsWithIntent interfaces work completion names [] Map.empty []
        case output of
          PortableOutputs _ -> fail "native package fixture selected portable output"
          NativeOutputs artifacts products -> do
            unless (length artifacts == length names) (fail "native package fixture lost its targets")
            forM_ artifacts $ \artifact -> unless
              (not (any ((== "reverse") . symbolOccurrence)
                [identity | group <- programBindings (paProgram artifact)
                  , TopBinding identity _ <- case group of NonRecursive top -> [top]; Recursive tops -> tops]))
              (fail "native target retained the package reverse implementation")
            void (writePreparedArtifacts work artifacts)
            void (writeCertifiedProductsKeepingWithOriginals [work] interfaces work prepared (Just products)
              [(paTarget artifact,paProgram artifact) | artifact <- artifacts])
            hasCode <- doesDirectoryExist (work </> "package-products")
            paths <- if hasCode then listDirectory (work </> "package-products") else pure []
            unless (not (null paths) == wantsCode)
              (fail "native writer confused TYPE support with executable package products")
            certificate <- BS.readFile (work </> "certified-products.cbor")
            forM_ paths $ \path -> do
              bytes <- BS.readFile (work </> "package-products" </> path)
              unless ("TPPACKAGEGROUP1" `BS.isInfixOf` bytes)
                (fail "native writer emitted an invalid package envelope")
              unless (TextEncoding.encodeUtf8 (Text.pack ("package-products/" ++ path)) `BS.isInfixOf` certificate)
                (fail "native certificate omitted an emitted package product")
            captured <- listDirectory (work </> "package-interfaces")
            unless (not (null captured)) (fail "native writer omitted captured package interfaces")
            packageRows <- case deserialiseFromBytes decodeTerm (BL.fromStrict certificate) of
              Right (rest,TList [TString "TPCERT",TInt 11,TList
                [TString "ordinary",_,_,TList rows,_,_,_,_,_]]) | BL.null rest -> pure rows
              _ -> fail "native writer emitted a malformed package witness table"
            witnesses <- forM packageRows $ \row -> case row of
              TList [TString unit,TString name,TString path,TString seal] -> do
                unless (takeDirectory (Text.unpack path) == work </> "package-interfaces")
                  (fail "selected package witness escaped its completed output")
                body <- captureArtifactBytes <$> BS.readFile (Text.unpack path)
                unless (Text.pack (artifactSha256 body) == seal)
                  (fail "selected package witness differs from its captured bytes")
                pure (unit,name,Text.unpack path,seal,body)
              _ -> fail "native writer emitted a malformed selected package witness"
            unless (wantsCode || any (\(_,name,_,_,_) -> name == "GHC.Types") witnesses)
              (fail "native package fixture lost its TYPE-only GHC.Types witness")
            unless (Set.fromList captured == Set.fromList
                [Text.unpack seal ++ ".hi" | (_,_,_,seal,_) <- witnesses])
              (fail "native package interfaces were duplicated or omitted from the witness table")
            case witnesses of
              (unit,name,_,seal,body):_ -> do
                let sourceCopy = work </> "captured-package-source.hi"
                    owner = (unit,name)
                    selected = [(unit,name,sourceCopy,seal)]
                    token = Map.singleton owner (sourceCopy,body)
                BS.writeFile sourceCopy (artifactBytes body)
                removeFile sourceCopy
                retained <- captureSelectedPackageWitnesses (work </> "retained-package-check") token (selected ++ selected)
                  >>= either fail pure
                unless (Map.size (fst retained) == 1 && length (snd retained) == 1)
                  (fail "selected package token was not shared across duplicate references")
                let refuse label result = case result of
                      Left _ -> pure ()
                      Right _ -> fail (label ++ " accepted invalid selected package custody")
                captureSelectedPackageWitnesses (work </> "missing-package-check") Map.empty selected
                  >>= refuse "missing interface"
                BS.writeFile sourceCopy (artifactBytes body)
                captureSelectedPackageWitnesses (work </> "forged-package-check") Map.empty
                  [(unit,name,sourceCopy,Text.replicate 64 "0")]
                  >>= refuse "forged seal"
                captureSelectedPackageWitnesses (work </> "conflicting-package-check") token
                  (selected ++ [(unit,name,sourceCopy,Text.replicate 64 "0")])
                  >>= refuse "conflicting owner"
              [] -> fail "native output omitted all selected package witnesses"

  where
    cases =
      [("NativePackageTarget",
        ["module NativePackageTarget where", "import Data.List (reverse)"
        ,"{-# NOINLINE __result #-}", "__result :: [Int] -> [Int]", "__result = reverse"],
        ["__result"],True)
      ,("NativeTypeSupport",
        ["{-# LANGUAGE NoImplicitPrelude #-}", "module NativeTypeSupport where"
        ,"data Token = First | Second", "first = First", "second = Second"],
        ["first","second"],False)]
