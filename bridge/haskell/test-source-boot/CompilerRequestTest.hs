{-# LANGUAGE ScopedTypeVariables #-}
module CompilerRequestTest (completedRequestHistories, completedRequestCancellation, completedRequestRelease, checkedPublicModuleHistories, worldRequestBoundaries, worldMaterializationBoundaries) where

import Tidepool.RequestCompletion
  ( finishCompletion, captureCompletion, completionResult, completionFailures
  , BodyOutcome(..), CleanupStage(..), cleanupStage, cleanupException, withCompletionRelease )

import Control.Exception (AsyncException(ThreadKilled), SomeException, SomeAsyncException, IOException, fromException, throwIO, try)
import Control.Concurrent (forkFinally, killThread, newEmptyMVar, putMVar, readMVar)
import Data.IORef (newIORef, readIORef, writeIORef)
import System.Timeout (timeout)
import Control.Monad ((>=>), forM_, unless, void)
import Control.Monad.IO.Class (liftIO)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.ByteString as BS
import GHC (findModule, getModuleInfo, modInfoExports, mkModuleName)
import GHC.Types.Name (getOccString)
import Tidepool.WorkerDiagnostics (sourceFailureDiagnostics)
import GHC.Tc.Types (tcg_mod)
import GHC.Unit.Module (moduleName, moduleNameString)
import SourceBootFixtureSupport (hasIntResultLiteral, withScratch)
import System.Directory (copyFile, createDirectory, doesDirectoryExist, doesFileExist, getFileSize, getPermissions, setPermissions, listDirectory, Permissions(..))
import System.FilePath ((</>), takeFileName)
import Test.QuickCheck
import Tidepool.CompilerProducts
  ( prepareCompilerProjectionContext, newPreparedOriginalInterfaceArtifacts, preparedProductInventory )
import Tidepool.CompilerRequest
import Tidepool.ExecutionProjection (preparedModuleProductOutcomes, projectOriginalHomeModuleProducts)
import Tidepool.ExtractRequest
import Tidepool.GhcPipeline
import Tidepool.DependencyEvidence (DependencyEvidence(..), DependencyModule(..))
import Tidepool.PackageWitness (requireObservationPackageEnvironment)
import Tidepool.ExactScope (decodeWorldSources)
import Tidepool.DiagJson (InputRejection(..))

worldRequestBoundaries :: IO ()
worldRequestBoundaries = do
  requireObservationPackageEnvironment
  let root = replicate 64 'a'
      resident = ExactInputsRequest root (ResidentExactInputs "foreign-worker-realization")
      complete = ExactInputsRequest root (CompleteExactInputs "/unavailable/world-inputs.cbor")
      question = WorldPublicModuleRequest root "foreign-worker-realization" WorldStructuredInfo
        (StructuredInspection (StructuredPublicModule "Data.List") StructuredValueName "map"
          (InspectionProvenance 1 "display-only")) "/unavailable/world-answer.bin"
      body = BS.pack [136,1,88,32] <> BS.replicate 32 170 <> BS.pack [0,105] <> "Data.List"
        <> BS.pack [132,1,99] <> "map" <> BS.pack [1,108] <> "display-only"
        <> BS.pack [130,104] <> "rejected" <> BS.pack [1,246,64]
      expected = "TPWQ0002" <> BS.pack [80,0,0,0] <> body
  args <- case workerRequestFromArgv (workerArgv [WorldPublicModule question]) of
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

-- These are raw closed-source data/I/O controls, not rooted world issuers.
-- They enter the same materialization owner used before world publication.
worldMaterializationBoundaries :: IO ()
worldMaterializationBoundaries = do
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
  (_,actual) <- prepareCompilationOriginals completion context Set.empty
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
