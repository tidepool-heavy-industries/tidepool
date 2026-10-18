module InspectionRunnerTest (inspectionRunnerChecks) where

import Control.Exception (AsyncException(..), Exception(..), asyncExceptionToException, asyncExceptionFromException, SomeAsyncException, SomeException, IOException, bracket, fromException, throwIO, toException, try)
import GHC.Types.SourceError (mkSrcErr)
import GHC.Types.Error (emptyMessages)
import Control.Monad (unless, forM_)
import Data.List (nub, nubBy, transpose, findIndex)
import qualified Test.QuickCheck as QC
import Data.IORef (newIORef, modifyIORef', readIORef, writeIORef)
import System.Directory (getTemporaryDirectory, removeFile)
import System.IO (hClose, openTempFile)
import System.IO.Error (isDoesNotExistError, isUserError, ioeGetErrorString, ioeGetFileName)
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), DependencyLoadFailure(..))
import Tidepool.ExtractRequest (RequestField(..), WorkerRequest(..), HostedLookupBatch(..), HostedLookupOperation(..), HostedLookupQuery(..), LookupNameInspection(..), LookupNameMode(..), InspectionRequest(..), ExactNameInspection(..), StructuredNameScope(..), StructuredNameNamespace(..), HostedLookupDiscovery(..), workerArgv, workerRequestFromArgv)
import Tidepool.GhcPipeline (CompilePurpose(..))
import Tidepool.InspectionRunner (runInspectionRequests, runHostedLookupRequests, HostedMetadataEnvironment(..))
import Tidepool.Introspection (InspectionResult(..), LookupAnswer(..), HostedLookupAnswer(..), HostedOperationIdentity(..), SelectedLookupAnswer(..), SelectedMatches(..), IdentifierRef(..), IdentifierNamespace(..), CandidateSeed(..), selectFairSeeds, finishHostedKnown, HostedLookupOutput(..), HostedDiscoverySelection(..), HostedFallback(..))

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
  let hosted = request [Input "metadata"]
        [HostedLookup (HostedLookupBatch
          [HostedLookupPrimary 1 "value" (HostedLookupName (LookupNameInspection "value" Nothing LookupNameOnly))]
          Nothing)]
  unsupported <- try (runInspectionRequests hosted record inspect) :: IO (Either IOException [InspectionResult])
  assert "generic complete entry refuses hosted execution before compiler work"
    (case unsupported of Left exception -> isUserError exception; Right _ -> False)
  beforeComplete <- readIORef calls
  beforeInspect <- readIORef inspected
  assert "unsupported hosted execution does not compile or inspect" (null beforeComplete && null beforeInspect)
  hostedRunnerChecks
  fairSelectionChecks
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

-- The real orchestration entry points use one request-local checked cohort.
-- Probe environments below are labels, not alternate compiler authorities.
hostedRunnerChecks :: IO ()
hostedRunnerChecks = do
  calls <- newIORef []
  finished <- newIORef []
  let check label ok = unless ok (fail ("hosted runner: " ++ label))
      name ordinal origin = HostedLookupPrimary ordinal origin (HostedLookupName (LookupNameInspection origin Nothing LookupNameOnly))
      ty = HostedLookupPrimary 7 ":: Int" (HostedLookupTypeSearch "Int")
      reference = HostedLookupExactReference [0,2] (ExactNameInspection StructuredCurrentScope StructuredValueName "Owner.value")
      policy = HostedLookupDiscovery 20 [StructuredCurrentScope] 0
      batch = HostedLookupBatch [name 1 "missing", ty, reference, name 9 "missing"] (Just policy)
      args = request [Input "metadata",Input "type",Input "metadata",Input "metadata"] [HostedLookup batch]
      record purpose path = modifyIORef' calls ((show purpose,path):) >> pure path
      inspect _path queries = pure [case query of
        InspectTypeSearch query' -> InspectionTypeMatches query' []
        InspectLookupNameOf query' -> InspectionLookup (lookupNameQuery query') [StructuredCurrentScope] (LookupMissing [])
        InspectExactNameOf query' -> InspectionLookup (exactNameQuery query') [exactNameScope query'] (LookupMissing [])
        _ -> error ("unexpected hosted operation " ++ show query) | query <- queries]
      finish metadata answers = do
        case metadata of
          HostedMetadataReady path -> modifyIORef' finished (("ready:" ++ path):)
          HostedMetadataRejected reason -> modifyIORef' finished (("rejected:" ++ reason):)
          NoHostedMetadata -> modifyIORef' finished ("none":)
        pure answers
  answers <- runHostedLookupRequests args record inspect finish
  history <- reverse <$> readIORef calls
  check "same metadata path compiled once; wildcard source is separate"
    (history == [(show GeneralCompile,"metadata"),(show LookupTypeCompile,"type")])
  check "original ordinal domains survive order, repeats and shared references" (answers ==
    [HostedNameAnswer 1 [StructuredCurrentScope] (SelectedMissing []), HostedTypeAnswer 7 (SelectedMatches 0 []),
     HostedReferenceAnswer [0,2] [StructuredCurrentScope] (SelectedMissing []), HostedNameAnswer 9 [StructuredCurrentScope] (SelectedMissing [])])
  check "finalizer retains exactly the admitted metadata completion" . (== ["ready:metadata"]) =<< readIORef finished
  writeIORef calls []
  writeIORef finished []
  let sourceFailure = DependencySourceFailure [Diag Nothing DiagError "metadata source rejected"]
      rejectMetadata purpose path = do
        environment <- record purpose path
        if path == "metadata" then throwIO sourceFailure else pure environment
  rejected <- runHostedLookupRequests args rejectMetadata inspect finish
  check "type success cannot substitute for failed metadata" . (== ["rejected:metadata source rejected"]) =<< readIORef finished
  check "source rejection reused for every metadata operation; type remains successful"
    (case rejected of [HostedRejected (HostedPrimaryIdentity 1) _, HostedTypeAnswer 7 _, HostedRejected (HostedReferenceIdentity [0,2]) _, HostedRejected (HostedPrimaryIdentity 9) _] -> True; _ -> False)
  rejectedHistory <- reverse <$> readIORef calls
  check "failed metadata is not recompiled or replayed" (history == rejectedHistory)
  writeIORef calls []
  writeIORef finished []
  let typeBatch = HostedLookupBatch [ty] (Just policy)
  typeAnswers <- runHostedLookupRequests (request [Input "type"] [HostedLookup typeBatch]) record inspect finish
  check "type-only discovery has no metadata owner" . (== ["none"]) =<< readIORef finished
  let typeOutput = finishHostedKnown typeBatch typeAnswers Nothing
  check "type-only output still has candidate receipt with no fallback"
    (case hostedLookupSelection typeOutput of Just selected -> hostedFallback selected == HostedFallbackNotNeeded; _ -> False)
  -- Reject multiple metadata paths at closed request admission, before compile.
  writeIORef calls []
  malformed <- try (runHostedLookupRequests (args { requestFiles = ["metadata","type","different","metadata"] }) record inspect finish)
    :: IO (Either IOException [HostedLookupAnswer])
  check "unrelated successful cohort cannot acquire discovery authority" (case malformed of Left _ -> True; _ -> False)
  check "invalid cohort has no compiler attempt" . null =<< readIORef calls
  -- Infrastructure and cancellation do not become source absence or partial output.
  forM_ [toException (userError "worker unavailable"), toException (TestCancellation "stopped")] $ \failure -> do
    writeIORef finished []
    failed <- try (runHostedLookupRequests args (\_ _ -> throwIO failure) inspect finish) :: IO (Either SomeException [HostedLookupAnswer])
    check "infrastructure or cancellation propagates" (case failed of Left _ -> True; _ -> False)
    check "failed physical request does not finalize discovery" . null =<< readIORef finished

-- Independent list/transpose oracle. Production uses Set/Map and an explicit
-- fair stream; the oracle selects its distinct prefix declaratively, then
-- accounts raw occurrences and admitted-first origin order separately.
fairSelectionChecks :: IO ()
fairSelectionChecks = do
  result <- QC.quickCheckWithResult QC.stdArgs { QC.maxSuccess = 500 } $
    QC.forAllShrink generator shrinker $ \(limit, rawGroups) ->
      let namespaces = [ValueIdentifier,TypeIdentifier,ConstructorIdentifier,FieldIdentifier]
          groups = [[CandidateSeed (show key) (Just (IdentifierRef (if namespace < 4 then "" else "Owner") (show key) (namespaces !! (namespace `mod` 4))))
                       ["origin" ++ show origin] True (show key) payload
                   | (key,namespace,origin,payload) <- group] | group <- rawGroups]
          identity seed = (seedQuery seed, seedReference seed)
          distinct = nubBy (\a b -> identity a == identity b)
          fair = concat (transpose (map distinct groups))
          chosen = take limit (distinct fair)
          keys = map identity chosen
          admitted = case reverse chosen of
            [] -> []
            lastChosen : _ -> maybe [] (\index -> take (index + 1) fair) (findIndex ((== identity lastChosen) . identity) fair)
          origins key = nub [origin | seed <- admitted ++ concat groups, identity seed == key, origin <- seedOrigins seed]
          expected = ([seed { seedOrigins = origins (identity seed) } | seed <- chosen], sum (map length groups),
            length [() | seed <- concat groups, identity seed `notElem` keys])
      in QC.checkCoverage
       $ QC.cover 30 (any (\group -> length (distinct group) < length group) groups) "duplicate addresses"
       $ QC.cover 30 (length (nub (map length groups)) > 1) "uneven groups"
       $ QC.cover 1 (limit == 0) "zero cap"
       $ QC.counterexample (show (limit,rawGroups)) (selectFairSeeds limit groups == expected)
  unless (QC.isSuccess result) (fail "mixed selector differs from independent prefix/origin/count oracle")
  let deferred = CandidateSeed "unused" Nothing ["origin"] True "unused" (error "unselected rich payload forced" :: String)
      first = CandidateSeed "first" Nothing ["origin"] True "first" "complete summary"
      (selected, count, discarded) = selectFairSeeds 1 [[first,deferred]]
  unless (map seedPayload selected == ["complete summary"] && count == 2 && discarded == 1)
    (fail "selector forced or admitted an unselected projection")
  forM_ [0,1,128] $ \cap -> do
    let (chosen,_,_) = selectFairSeeds cap [[first,deferred]]
    unless (length chosen == min cap 2) (fail "mixed selector cap boundary")
  where
    generator = do
      limit <- QC.chooseInt (0,20)
      groups <- QC.listOf (QC.resize 12 (QC.listOf ((,,,) <$> QC.chooseInt (0,8) <*> QC.chooseInt (0,7) <*> QC.chooseInt (0,4) <*> QC.chooseInt (0,100))))
      pure (limit,groups)
    shrinker (limit,groups) = [(limit',groups') | (limit',groups') <- QC.shrink (limit,groups), limit' >= 0]
