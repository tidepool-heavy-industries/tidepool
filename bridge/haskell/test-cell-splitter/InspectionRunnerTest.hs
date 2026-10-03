module InspectionRunnerTest (inspectionRunnerChecks) where

import Control.Exception (AsyncException(..), Exception(..), asyncExceptionToException, asyncExceptionFromException, SomeAsyncException, SomeException, IOException, bracket, fromException, throwIO, toException, try)
import GHC.Types.SourceError (mkSrcErr)
import GHC.Types.Error (emptyMessages)
import Control.Monad (unless)
import Data.IORef (newIORef, modifyIORef', readIORef, writeIORef)
import System.Directory (getTemporaryDirectory, removeFile)
import System.IO (hClose, openTempFile)
import System.IO.Error (isDoesNotExistError, isUserError, ioeGetErrorString, ioeGetFileName)
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), DependencyLoadFailure(..))
import Tidepool.ExtractRequest (RequestField(..), WorkerRequest(..), workerArgv, workerRequestFromArgv)
import Tidepool.GhcPipeline (CompilePurpose(..))
import Tidepool.InspectionRunner (runInspectionRequests)
import Tidepool.Introspection (InspectionResult(..))

newtype TestCancellation = TestCancellation String deriving Show
instance Exception TestCancellation where
  toException = asyncExceptionToException
  fromException = asyncExceptionFromException

inspectionRunnerChecks :: IO ()
inspectionRunnerChecks = do
  calls <- newIORef []
  inspected <- newIORef []
  let assert label ok = unless ok (fail ("inspection runner: " ++ label))
      record purpose path = modifyIORef' calls ((show purpose, path) :) >> pure path
      inspect path queries = do
        modifyIORef' inspected ((path, queries) :)
        pure [InspectionNotFound (path ++ ":" ++ show query) | query <- queries]
      sourceFailure = DependencySourceFailure [Diag (Just ("bad.hs", 2, 3, 2, 4)) DiagError "bad source"]
      compile purpose path = do
        environment <- record purpose path
        if path == "bad" then throwIO sourceFailure else pure environment
      mixed = request [Input "same", Input "same", Input "same", Input "other", Input "bad", Input "bad"]
        [InspectType "a", InspectInfo "name", InspectSearch "_", InspectType "b", InspectInfo "x", InspectType "c"]
  answers <- runInspectionRequests mixed compile inspect
  history <- reverse <$> readIORef calls
  assert "memo distinguishes exact paths and wildcard policy; source failures are reused"
    (history == [(show GeneralCompile,"same"),(show LookupTypeCompile,"same"),
      (show GeneralCompile,"other"),(show GeneralCompile,"bad")])
  assert "mixed result order survives cached source rejection"
    (answers == [InspectionNotFound (path ++ ":" ++ show query)
      | (path,query) <- take 4 (zip (requestFiles mixed) (requestInspections mixed))]
      ++ replicate 2 (InspectionRejected "bad.hs:2:3: bad source"))
  writeIORef calls []
  _ <- runInspectionRequests mixed compile inspect
  repeatedCalls <- readIORef calls
  assert "memo does not escape the current request" (length repeatedCalls == 4)
  visits <- reverse . take 4 <$> readIORef inspected
  assert "singleton inspection keeps each query's own probe numbering"
    (visits == zip ["same","same","same","other"] (map (:[]) (take 4 (requestInspections mixed))))
  writeIORef calls []
  stopped <- try (runInspectionRequests (mixed { requestInspectionStrict = True }) compile inspect)
  assert "strict source rejection propagates" (isDependencySource stopped)
  historyAfterStrict <- reverse <$> readIORef calls
  assert "strict rejection stops at first failed source" (length historyAfterStrict == 4)
  withBatch $ \batch -> do
    let batched = (request [Input "first", Input "second"] [InspectType "a", InspectType "b"])
          { requestInspectTypeBatch = Just batch }
        reset = writeIORef calls [] >> writeIORef inspected []
        rejectBatch :: Exception e => e -> CompilePurpose -> FilePath -> IO FilePath
        rejectBatch exception purpose path = do
          value <- record purpose path
          if path == batch then throwIO exception else pure value
    reset
    successful <- runInspectionRequests batched record inspect
    successfulCalls <- reverse <$> readIORef calls
    assert "successful batch compiles once and preserves all type probes"
      (successfulCalls == [(show GeneralCompile,batch)] && length successful == 2)
    batchVisits <- readIORef inspected
    assert "type probe indexing belongs to the complete successful batch"
      (batchVisits == [(batch,requestInspections batched)])
    reset
    _ <- runInspectionRequests batched (rejectBatch sourceFailure) inspect
    fallbackCalls <- reverse <$> readIORef calls
    assert "authored dependency rejection falls back in source order"
      (map snd fallbackCalls == [batch,"first","second"])
    reset
    _ <- runInspectionRequests batched (rejectBatch (mkSrcErr emptyMessages)) inspect
    directSourceCalls <- reverse <$> readIORef calls
    assert "direct GHC source rejection also permits fallback"
      (map snd directSourceCalls == [batch,"first","second"])
    reset
    inspectorFailure <- try (runInspectionRequests batched record
      (\_ _ -> throwIO sourceFailure)) :: IO (Either SomeException [InspectionResult])
    assert "inspector rejection is not a batch compilation fallback" (isDependencySource inspectorFailure)
    inspectorCalls <- readIORef calls
    assert "inspector failure leaves singleton sources untouched" (length inspectorCalls == 1)
    reset
    strictBatch <- try (runInspectionRequests (batched { requestInspectionStrict = True }) (rejectBatch sourceFailure) inspect)
    assert "strict batch rejects without fallback" (isDependencySource strictBatch)
    strictCalls <- readIORef calls
    assert "strict batch does not compile singletons" (length strictCalls == 1)
    mapM_ (\exception -> do
      reset
      failed <- try (runInspectionRequests batched (rejectBatch exception) inspect) :: IO (Either SomeException [InspectionResult])
      failureCalls <- readIORef calls
      assert "non-source failure compiles only batch" (length failureCalls == 1)
      case fromException exception :: Maybe SomeAsyncException of
        Just _ -> assert "async exception retains its category" (either (maybe False (const True) . (fromException :: SomeException -> Maybe SomeAsyncException)) (const False) failed)
        Nothing -> assert "worker failure retains the infrastructure category"
          (either (\caught -> case fromException caught of
            Just DependencyWorkerFailure -> True
            _ -> False) (const False) failed))
      [toException DependencyWorkerFailure, toException ThreadKilled, toException (TestCancellation "cancel")]
    reset
    missing <- try (runInspectionRequests (batched {requestInspectTypeBatch = Just (batch ++ ".missing")}) record inspect) :: IO (Either SomeException [InspectionResult])
    assert "missing batch file retains filesystem error and path"
      (isIOFailure (\exception -> isDoesNotExistError exception
        && ioeGetFileName exception == Just (batch ++ ".missing")) missing)
    missingCalls <- readIORef calls
    assert "unreadable producer has no compiler calls" (null missingCalls)
    reset
    invalid <- try (runInspectionRequests (mixed {requestInspectTypeBatch = Just batch}) record inspect) :: IO (Either SomeException [InspectionResult])
    assert "mixed batches report the batch admission error"
      (isUserFailure "inspection type batch requires at least two type queries and no other query kinds" invalid)
    invalidCalls <- readIORef calls
    assert "invalid batch has no compiler calls" (null invalidCalls)
  let mismatched = mixed { requestFiles = [] }
  writeIORef calls []
  mismatch <- try (runInspectionRequests mismatched record inspect) :: IO (Either SomeException [InspectionResult])
  assert "source/query count mismatch reports the shape admission error"
    (isUserFailure "inspection request must carry exactly one source per query" mismatch)
  mismatchCalls <- readIORef calls
  assert "count mismatch has no compiler calls" (null mismatchCalls)
  where
    isIOFailure :: (IOException -> Bool) -> Either SomeException [InspectionResult] -> Bool
    isIOFailure predicate (Left exception) = maybe False predicate (fromException exception)
    isIOFailure _ _ = False
    isUserFailure message = isIOFailure
      (\exception -> isUserError exception && ioeGetErrorString exception == message)
    isDependencySource :: Either SomeException [InspectionResult] -> Bool
    isDependencySource (Left exception) = case fromException exception of
      Just (DependencySourceFailure _) -> True
      _ -> False
    isDependencySource _ = False

request :: [RequestField] -> [RequestField] -> WorkerRequest
request sources queries = case workerRequestFromArgv (workerArgv (sources ++ queries)) of
  Right (Just value) -> value
  other -> error ("inspection fixture request: " ++ show other)

withBatch :: (FilePath -> IO a) -> IO a
withBatch action = do
  directory <- getTemporaryDirectory
  bracket (do (path, handle) <- openTempFile directory "inspection-runner-batch"; hClose handle; pure path)
    removeFile action
