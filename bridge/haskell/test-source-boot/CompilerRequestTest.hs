{-# LANGUAGE ScopedTypeVariables #-}
module CompilerRequestTest (completedRequestHistories, completedRequestCancellation, completedRequestRelease) where

import Tidepool.RequestCompletion (finishCompletion)

import Control.Exception (AsyncException(ThreadKilled), SomeException, SomeAsyncException, fromException, throwIO, try)
import Control.Monad ((>=>), forM_, unless, void)
import Control.Monad.IO.Class (liftIO)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC (findModule, getModuleInfo, modInfoExports, mkModuleName)
import GHC.Types.Name (getOccString)
import Tidepool.WorkerDiagnostics (sourceFailureDiagnostics)
import GHC.Tc.Types (tcg_mod)
import GHC.Unit.Module (moduleName, moduleNameString)
import SourceBootFixtureSupport (hasIntResultLiteral, withScratch)
import System.Directory (copyFile)
import System.FilePath ((</>))
import Test.QuickCheck
import Tidepool.CompilerProducts
  ( prepareCompilerProjectionContext, newPreparedOriginalInterfaceArtifacts, preparedProductInventory )
import Tidepool.CompilerRequest
import Tidepool.ExecutionProjection (preparedModuleProductOutcomes, projectOriginalHomeModuleProducts)
import Tidepool.ExtractRequest
import Tidepool.GhcPipeline

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
