{-# LANGUAGE ScopedTypeVariables #-}

module WorkerResponseCases where

import Control.Exception (AsyncException(..), IOException, SomeException, bracket, catch, finally, throwIO, try)
import Control.Monad (unless, void, forM_, replicateM, when)
import Control.Concurrent (ThreadId, MVar, forkFinally, killThread, newEmptyMVar, putMVar, takeMVar, threadDelay)
import Tidepool.DependencyPlan (sourceDependencyQuery, nativeDependencyQuery, exchangeSourceDependencies, exchangeNativeDependencies)
import Data.Bits ((.|.), shiftL, shiftR)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BSC
import Data.Word (Word8, Word32, Word64)
import Data.List (isPrefixOf, isInfixOf, sort)
import Data.IORef (newIORef, readIORef, writeIORef, atomicModifyIORef')
import System.Directory (createDirectory, doesFileExist, getTemporaryDirectory, listDirectory, removeFile, removeDirectoryRecursive)
import System.Environment (lookupEnv, setEnv)
import System.Exit (ExitCode(..))
import System.IO (Handle, hClose, hFlush, openTempFile, stdin, stdout, stderr)
import System.Process (ProcessHandle, CreateProcess(..), StdStream(..), createProcess, proc, terminateProcess, waitForProcess, readProcessWithExitCode)
import System.IO.Error (isFullError)
import System.Timeout (timeout)
import System.Posix.Files (setFileMode)
import GHC.Conc (getNumCapabilities, ThreadStatus(..), BlockReason(..), threadStatus)
import GHC.Clock (getMonotonicTimeNSec)
import Tidepool.CompilerRequest (withBoundCompilerRequest, compileBoundRequest)
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), SourceRejection(..))
import Tidepool.ExtractRequest
  ( AdmittedWorkerRequest, admitWorkerRequest, WorkerRequest, workerRequestFromArgv, workerArgv, RequestField(..)
  , EnsureExactInputsOutcome(..), ExactInputsRejection(..), WorldResidency(..)
  , WorldAcquisitionMiss(..), encodeEnsureExactInputsOutcomeWithin )
import Tidepool.GhcPipeline
  ( withResidentCompilerScopes, PipelineSelection(CheckedEnvironment), CompilePurpose(GeneralCompile) )
import System.Posix.Resource
  ( Resource(..), ResourceLimit(..), ResourceLimits(..), getResourceLimit, setResourceLimit )
import GHC.IO.Handle (hDuplicate)
import Tidepool.WorkerServer (WorkerResponse(..), runWorkerLoop, NativeDemandReply(..), exchangeNativeDemand, requireNativeDemandOwner, borrowSourceDemand, exchangeSourceDemand)
import Tidepool.RequestCompletion
  ( captureCompletion, flattenCompletion, finishCompletion )

limit :: Int
limit = 16777216

successReport :: BS.ByteString
successReport = BSC.pack "{\"version\":2,\"outcome\":\"success\",\"diagnostics\":[]}\n"

workerChild :: Bool -> FilePath -> IO ()
workerChild oneFreeFd scratch = do
  stale <- newIORef Nothing
  setEnv "TMPDIR" scratch
  if oneFreeFd
    then do
      first <- newIORef True
      runWorkerLoop $ \transaction -> do
        limited <- atomicModifyIORef' first (\isFirst -> (False, isFirst))
        if limited
          then withOneFreeFd (transaction (handler stale) `catch` catchResourceExhaustion)
          else transaction (handler stale) `catch` catchThreadKilled
    else runWorkerLoop (\transaction -> transaction (handler stale) `catch` catchThreadKilled)
  where
    handler stale _ native _ args = captureCompletion $ do
      case args of
        ["source-concurrent"] -> do
          exchange <- maybe (fail "missing SOURCE exchange") pure native
          borrowed <- borrowSourceDemand exchange 3
          writeIORef stale (Just borrowed)
          -- Original API stays handler-only, even with a live SOURCE borrow.
          direct <- childResult (exchangeNativeDemand exchange 3 "foreign-thread")
          expectFailure "NativeDemandWrongThread" direct
          jobs <- mapM (startChild . exchangeSourceDemand borrowed . sourcePayload) ["left","right","left"]
          forM_ jobs $ \(_,done) -> takeMVar done >>= either throwIO (\reply ->
            unless (case reply of NativeResolved body -> body == "observed"; _ -> False)
              (fail "concurrent SOURCE reply changed"))
          actual <- exchangeSourceDependencies borrowed (sourceDependencyQuery [])
          unless (null actual) (fail "empty actual SOURCE query acquired rows")
        ["source-stale"] -> do
          old <- readIORef stale >>= maybe (fail "missing retained SOURCE borrow") pure
          childResult (exchangeSourceDemand old (sourcePayload "stale")) >>= expectFailure "NativeDemandClosed"
          exchange <- maybe (fail "missing next SOURCE exchange") pure native
          borrowed <- borrowSourceDemand exchange 4
          newer <- borrowSourceDemand exchange 5
          childResult (exchangeSourceDemand borrowed (sourcePayload "old-stage")) >>= expectFailure "NativeDemandClosed"
          _ <- exchangeSourceDemand newer (sourcePayload "current")
          pure ()
        ["source-kind"] -> do
          exchange <- maybe (fail "missing SOURCE kind exchange") pure native
          _ <- exchangeNativeDependencies exchange 0 (nativeDependencyQuery [])
          -- Echo the real native encoder back as a body, then try it through
          -- the borrowed SOURCE edge. No duplicate native query may be sent.
          encoded <- exchangeNativeDemand exchange 0 "request-native-encoding"
          bytes <- case encoded of NativeResolved body -> pure body; _ -> fail "missing native encoding"
          borrowed <- borrowSourceDemand exchange 0
          childResult (exchangeSourceDemand borrowed bytes) >>= expectFailure "SOURCE borrow requires"
          pure ()
        ["source-semantic"] -> do
          exchange <- maybe (fail "missing semantic SOURCE exchange") pure native
          borrowed <- borrowSourceDemand exchange 0
          childResult (exchangeSourceDependencies borrowed (sourceDependencyQuery [])) >>= expectFailure "NativeReplyCorrupt"
          actual <- exchangeSourceDependencies borrowed (sourceDependencyQuery [])
          unless (null actual) (fail "corrupt semantic reply acquired rows")
        ["source-refusal"] -> do
          exchange <- maybe (fail "missing refusing SOURCE exchange") pure native
          old <- borrowSourceDemand exchange 2
          borrowed <- borrowSourceDemand exchange 3
          writeIORef stale (Just borrowed)
          (_,firstDone) <- startChild (exchangeSourceDependencies borrowed (sourceDependencyQuery []))
          awaitFile (scratch ++ "/refusal-frame")
          (valid,validDone) <- startChild (exchangeSourceDependencies borrowed (sourceDependencyQuery []))
          (invalid,invalidDone) <- startChild (exchangeSourceDemand old (sourcePayload "stale"))
          awaitGateWait valid
          awaitGateWait invalid
          writeFile (scratch ++ "/refusal-waiters") "ready"
          forM_ [firstDone,validDone] $ \done ->
            takeMVar done >>= expectFailure "NativeDemandRejected OriginalUnavailable \"first-diagnostic\""
          takeMVar invalidDone >>= expectFailure "NativeDemandClosed"
          wrongOwner <- try (requireNativeDemandOwner exchange
            (BS.replicate 32 9) (BS.replicate 32 3) (BS.replicate 32 4))
          expectFailure "native demand owner differs from admitted World inputs" wrongOwner
          childResult (exchangeNativeDemand exchange 3 "foreign") >>= expectFailure "NativeDemandWrongThread"
          childResult (exchangeSourceDemand borrowed (dependencyPacket "TPDEPENDENCYQUERY1" 1))
            >>= expectFailure "SOURCE borrow requires"
          regressed <- try (exchangeNativeDemand exchange 2 "regressed")
          expectFailure "native demand stage regressed" regressed
          regressedBorrow <- try (borrowSourceDemand exchange 2)
          expectFailure "SOURCE borrow stage regressed" regressedBorrow
          -- A replay neither reserves another round nor consumes wire budget.
          forM_ [1 :: Int,2] $ \_ -> do
            reply <- exchangeNativeDemand exchange 5 (BS.replicate limit 114)
            unless (case reply of NativeRefused bytes -> bytes == firstRefusal; _ -> False)
              (fail "handler replay lost first opaque refusal")
          replayRegressed <- try (exchangeNativeDemand exchange 4 "regressed-after-replay")
          expectFailure "native demand stage regressed" replayRegressed
          childResult (exchangeSourceDemand borrowed (sourcePayload "old-demand-stage"))
            >>= expectFailure "native demand stage regressed"
          newer <- borrowSourceDemand exchange 5
          childResult (exchangeSourceDemand borrowed (sourcePayload "old-borrow-stage"))
            >>= expectFailure "NativeDemandClosed"
          childResult (exchangeSourceDependencies newer (sourceDependencyQuery []))
            >>= expectFailure "NativeDemandRejected OriginalUnavailable \"first-diagnostic\""
          _ <- exchangeNativeDependencies exchange 5 (nativeDependencyQuery [])
          fail "refusal unexpectedly resolved"
        ["refusal-malformed"] -> do
          exchange <- maybe (fail "missing malformed refusal exchange") pure native
          borrowed <- borrowSourceDemand exchange 0
          forM_ [1 :: Int,2] $ \_ ->
            childResult (exchangeSourceDependencies borrowed (sourceDependencyQuery [])) >>= expectFailure "NativeReplyCorrupt"
          fail "malformed refusal retained"
        [mode] | mode `elem` ["refusal-error","refusal-returned-error","refusal-warning","refusal-swallowed","refusal-render-failure"] -> do
          exchange <- maybe (fail "missing terminal refusal exchange") pure native
          _ <- exchangeNativeDemand exchange 0 "terminal-refusal"
          when (mode == "refusal-error") (throwIO (SourceRejection "typed-source-rejection"))
          when (mode == "refusal-render-failure") (BS.hPut stdout (BS.replicate limit 111))
        ["diagnostic-error"] -> pure ()
        ["diagnostic-warning"] -> pure ()
        ["source-cancel"] -> do
          exchange <- maybe (fail "missing cancellation SOURCE exchange") pure native
          borrowed <- borrowSourceDemand exchange 0
          (active,activeDone) <- startChild (exchangeSourceDemand borrowed (sourcePayload "blocked-read"))
          -- Parent creates this only after observing the entire first frame.
          let awaitTrigger = do
                ready <- doesFileExist (scratch ++ "/cancel-source")
                unless ready (threadDelay 1000 >> awaitTrigger)
          awaitTrigger
          (waiting,waitingDone) <- startChild (exchangeSourceDemand borrowed (sourcePayload "blocked-gate"))
          blocked <- timeout 1000000 (awaitGateWait waiting)
          unless (blocked == Just ()) (fail "SOURCE gate waiter never blocked on its actual gate")
          killThread waiting
          takeMVar waitingDone >>= expectFailure "thread killed"
          killThread active
          takeMVar activeDone >>= expectFailure "thread killed"
          childResult (exchangeSourceDemand borrowed (sourcePayload "after-cancel")) >>= expectFailure "NativeDemandClosed"
          removeFile (scratch ++ "/cancel-source")
        ["source-partial"] -> do
          exchange <- maybe (fail "missing partial SOURCE exchange") pure native
          borrowed <- borrowSourceDemand exchange 0
          childResult (exchangeSourceDemand borrowed (sourcePayload "partial")) >>= expectFailure "truncated frame"
          childResult (exchangeSourceDemand borrowed (sourcePayload "after-partial")) >>= expectFailure "NativeDemandClosed"
        ["native-exchange"] -> do
          exchange <- maybe (fail "exchange-capable request lost its active channel") pure native
          requireNativeDemandOwner exchange (BS.replicate 32 2) (BS.replicate 32 3) (BS.replicate 32 4)
          first <- exchangeNativeDemand exchange 3 "first-root"
          unless (case first of NativeResolved bytes -> bytes == "first-product"; _ -> False)
            (fail "first native exchange returned another product")
          second <- exchangeNativeDemand exchange 4 "second-root"
          unless (case second of NativeResolved bytes -> bytes == "second-product"; _ -> False)
            (fail "second native exchange returned another product")
        ["native-budget"] -> do
          exchange <- maybe (fail "native budget lost its active channel") pure native
          _ <- exchangeNativeDemand exchange 0 "small"
          pure ()
        ["native-refusal"] -> do
          exchange <- maybe (fail "native refusal lost its active channel") pure native
          reply <- exchangeNativeDemand exchange 0 "refused-root"
          case reply of
            NativeRefused bytes | bytes == "actual-semantic-refusal" -> fail "actual native semantic refusal"
            _ -> fail "native semantic refusal became successful resolution"
        ["source-free"] -> BSC.hPutStr stdout "captured-control-debug"
        ["source-free-rejected"] -> pure ()
        ["source-free-capture-overflow"] -> BS.hPut stdout (BS.replicate (limit + 1) 111)
        ["source-free-unbounded-reference"] -> pure ()
        ["source-free-capture-failure"] -> removeCaptureFiles scratch
        ["source-free-rejected-capture-failure"] -> removeCaptureFiles scratch
        ["stdout-overflow"] -> BS.hPut stdout (BS.replicate (limit + 1) 111)
        ["exact-aggregate"] -> do
          BS.hPut stdout (BS.replicate (limit `div` 2) 111)
          BS.hPut stderr (BS.replicate (limit - limit `div` 2 - BS.length successReport) 101)
        ["aggregate-overflow"] -> do
          BS.hPut stdout (BS.replicate (limit `div` 2) 111)
          BS.hPut stderr (BS.replicate (limit - limit `div` 2 - BS.length successReport + 1) 101)
        ["throw"] -> do
          BSC.hPutStr stdout "before-async-exception"
          BSC.hPutStr stderr "before-async-exception"
          throwIO ThreadKilled
        ["next-request"] -> BSC.hPutStr stdout "complete-next-request"
        _ -> fail "unexpected capture fixture request"
      pure $ case args of
        ["diagnostic-error"] -> DiagnosticResponse [Diag Nothing DiagError "typed-diagnostic-error"]
        ["diagnostic-warning"] -> DiagnosticResponse [Diag Nothing DiagWarning "typed-diagnostic-warning"]
        ["refusal-returned-error"] -> DiagnosticResponse [Diag Nothing DiagError "typed-diagnostic-error"]
        ["refusal-warning"] -> DiagnosticResponse [Diag Nothing DiagWarning "typed-diagnostic-warning"]
        ["source-free"] -> ExactInputsResponse (ExactInputsNeedsAcquisition (replicate 64 'a') WorldNotResidentOnThisWorker)
        ["source-free-rejected"] -> ExactInputsResponse (ExactInputsRejected AcquisitionRejected)
        ["source-free-capture-overflow"] -> ExactInputsResponse (ExactInputsReady (replicate 64 'a') (replicate 64 '1' ++ ".7") WorldRetainedAtAdmission)
        ["source-free-unbounded-reference"] -> ExactInputsResponse (ExactInputsReady (replicate 64 'a') (repeat 'x') WorldRetainedAtAdmission)
        ["source-free-capture-failure"] -> ExactInputsResponse (ExactInputsReady (replicate 64 'a') (replicate 64 '1' ++ ".7") WorldRetainedAtAdmission)
        ["source-free-rejected-capture-failure"] -> ExactInputsResponse (ExactInputsRejected AcquisitionRejected)
        _ -> DiagnosticResponse []

awaitFile :: FilePath -> IO ()
awaitFile path = do
  present <- doesFileExist path
  unless present (threadDelay 1000 >> awaitFile path)

firstRefusal :: BS.ByteString
firstRefusal = BS.singleton 0x83 <> cborText "TPNATIVEREFUSAL1" <> BS.singleton 1 <> cborText "first-diagnostic"

-- Protocol fixtures use semantically inert payloads for transport scheduling;
-- actual DependencyPlan encoders are checked independently in the same run.
sourcePayload :: BS.ByteString -> BS.ByteString
sourcePayload label = BS.singleton 0x83 <> cborText "TPDEPENDENCYQUERY1" <> BS.singleton 0 <> cborText label

-- Fixed short test strings form an independent wire oracle, not a second
-- production dependency codec.
cborText :: BS.ByteString -> BS.ByteString
cborText body
  | BS.length body < 24 = BS.singleton (0x60 + fromIntegral (BS.length body)) <> body
  | otherwise = error "SOURCE protocol fixture text is no longer short"

awaitGateWait :: ThreadId -> IO ()
awaitGateWait thread = threadStatus thread >>= \status -> case status of
  ThreadBlocked BlockedOnMVar -> pure ()
  ThreadFinished -> fail "SOURCE gate waiter finished before cancellation"
  ThreadDied -> fail "SOURCE gate waiter died before cancellation"
  _ -> threadDelay 1000 >> awaitGateWait thread

startChild :: IO a -> IO (ThreadId, MVar (Either SomeException a))
startChild action = do
  done <- newEmptyMVar
  thread <- forkFinally action (putMVar done)
  pure (thread,done)

childResult :: IO a -> IO (Either SomeException a)
childResult action = startChild action >>= takeMVar . snd

expectFailure :: String -> Either SomeException a -> IO ()
expectFailure expected result = case result of
  Left failure | expected `isInfixOf` show failure -> pure ()
  Left failure -> fail ("unexpected child failure: " ++ show failure)
  Right _ -> fail ("child unexpectedly succeeded: " ++ expected)

check :: FilePath -> IO ()
check scratch = do
  executable <- lookupEnv "TIDEPOOL_TEST_WORKER_RESPONSE_CHILD" >>= maybe
    (fail "missing declared worker-response child executable") pure
  bracket (createProcess (proc executable ["--worker", scratch])
      { std_in = CreatePipe, std_out = CreatePipe, std_err = Inherit })
    (\(_, _, _, child) -> terminateProcess child >> void (waitForProcess child)) $
    \(inputHandle, outputHandle, _, child) -> do
      input <- maybe (fail "missing worker stdin pipe") pure inputHandle
      output <- maybe (fail "missing worker stdout pipe") pure outputHandle
      BS.hPut input (BS.singleton 1)
      hFlush input
      ack <- exact output 1
      unless (ack == BS.singleton 1) (fail "missing transaction acknowledgement")
      forRequest input output "source-free" $ \code out err ->
        unless (code == 0
            && Just out == encodeEnsureExactInputsOutcomeWithin (toInteger limit) (ExactInputsNeedsAcquisition (replicate 64 'a') WorldNotResidentOnThisWorker)
            && err == "captured-control-debug")
          (fail "typed source-free result did not use the machine channel")
      forRequest input output "source-free-capture-overflow" $ \code out _ ->
        unless (code == 1 && BSC.isInfixOf "worker-failure" out
            && not (BSC.isInfixOf "TPEXO" out))
          (fail "capture overflow exposed an unconfirmed Ready result")
      forRequest input output "source-free-unbounded-reference" $ \code out err ->
        unless (code == 1 && BSC.isInfixOf "worker-failure" out
            && BS.length out < 1024 && BS.null err)
          (fail "typed response encoding forced or admitted an unbounded invalid reference")
      forRequest input output "source-free-rejected" $ \code out err ->
        unless (code == 0
            && Just out == encodeEnsureExactInputsOutcomeWithin (toInteger limit) (ExactInputsRejected AcquisitionRejected)
            && BS.null err)
          (fail "typed acquisition refusal was confused with transport failure")
      forRequest input output "stdout-overflow" $ \code out err -> do
        unless (code == 1 && BSC.isInfixOf "worker-failure" out && BS.length err < 1024)
          (fail "oversized stdout did not produce bounded infrastructure failure")
        unless (BS.length out < 1024) (fail "overflow diagnostic is not bounded")
      forRequest input output "exact-aggregate" $ \code out err ->
        unless (code == 0 && out == successReport
            && err == BS.replicate (limit `div` 2) 111
              <> BS.replicate (limit - limit `div` 2 - BS.length successReport) 101
            && BS.length out + BS.length err == limit)
          (fail "exact aggregate boundary was truncated or refused")
      forRequest input output "aggregate-overflow" $ \code out err ->
        unless (code == 1 && BSC.isInfixOf "worker-failure" out && BS.length out < 1024 && BS.null err)
          (fail "aggregate capture overflow was not refused")
      forRequest input output "next-request" $ \code out err ->
        unless (code == 0 && out == successReport && err == "complete-next-request")
          (fail "worker did not continue after oversized capture")
      let header = "TPNDX001" <> BS.replicate 32 1 <> BS.replicate 32 2
            <> BS.replicate 32 3 <> BS.replicate 32 4 <> put64 999999999999999999
          beginNative label = do
            BS.hPut input (BS.singleton 6 <> header <> frame "." <> put32 1 <> frame label)
            hFlush input
          answer stage sequenceNumber wanted outcome reply = do
            observed <- exact output 161
            unless (observed == BS.singleton 4 <> header <> put64 stage <> put64 sequenceNumber)
              (fail "native event lost actual request/stage/sequence correlation")
            payload <- readFrame output
            unless (payload == wanted) (fail "native event payload changed through captured stdout")
            BS.hPut input (BS.singleton 5 <> header <> put64 stage <> put64 sequenceNumber
              <> BS.singleton outcome <> frame reply)
            hFlush input
          terminal expected = do
            tag <- exact output 1
            code <- word32 <$> exact output 4
            out <- readFrame output
            err <- readFrame output
            unless (tag == BS.singleton 0 && code == expected
                && if expected == 0 then out == successReport && BS.null err
                  else BSC.isInfixOf "actual native semantic refusal" out)
              (fail "native exchange exposed a premature or incorrect terminal result")
      beginNative "native-exchange"
      answer 3 1 "first-root" 0 "first-product"
      answer 4 2 "second-root" 0 "second-product"
      terminal 0
      beginNative "native-refusal"
      answer 0 1 "refused-root" 1 "actual-semantic-refusal"
      terminal 1
      forRequest input output "next-request" $ \code out err ->
        unless (code == 0 && out == successReport && err == "complete-next-request")
          (fail "semantic native refusal poisoned ordinary request completion")
      BS.hPut input (BS.singleton 0)
      hFlush input
      ackDone <- exact output 1
      unless (ackDone == BS.singleton 1) (fail "missing completed transaction acknowledgement")

      BS.hPut input (BS.singleton 1)
      hFlush input
      ackNext <- exact output 1
      unless (ackNext == BS.singleton 1) (fail "missing next transaction acknowledgement")
      BS.hPut input (BS.singleton 1 <> frame "." <> put32 1 <> frame "throw")
      hFlush input
      ackAborted <- timeout 10000000 (exact output 1)
      unless (ackAborted == Just (BS.singleton 1))
        (fail "worker did not leave the failed transaction")

      BS.hPut input (BS.singleton 1)
      hFlush input
      ackRecovered <- exact output 1
      unless (ackRecovered == BS.singleton 1) (fail "worker did not accept a later transaction")
      forRequest input output "next-request" $ \code out err ->
        unless (code == 0 && out == successReport && err == "complete-next-request")
          (fail "capture was not restored after an async handler exception")
      BS.hPut input (BS.singleton 0)
      hFlush input
      ackFinal <- exact output 1
      unless (ackFinal == BS.singleton 1) (fail "missing recovered transaction acknowledgement")
      hClose input
      status <- waitForProcess child
      unless (status == ExitSuccess) (fail "worker failed to close cleanly")
      leftovers <- listDirectory scratch
      unless (null leftovers) (fail ("capture tempfiles retained: " ++ show leftovers))
      putStrLn "worker response bounds and cleanup: 1 passed"

-- Real protocol channels must retire malformed accepted framing without
-- waiting for EOF or exposing an unconfirmed terminal result.
checkNativeExchangeFailures :: FilePath -> IO ()
checkNativeExchangeFailures scratch = do
  executable <- lookupEnv "TIDEPOOL_TEST_WORKER_RESPONSE_CHILD" >>= maybe
    (fail "missing declared worker-response child executable") pure
  forM_ ["owner","stage","sequence","outcome","oversize","cumulative"] $ \mode ->
    bracket (createProcess (proc executable ["--worker",scratch])
        {std_in=CreatePipe,std_out=CreatePipe,std_err=Inherit})
      (\(_,_,_,child) -> terminateProcess child >> void (waitForProcess child)) $
      \(inputHandle,outputHandle,_,child) -> do
        input <- maybe (fail "missing native refusal stdin") pure inputHandle
        output <- maybe (fail "missing native refusal stdout") pure outputHandle
        BS.hPut input (BS.singleton 1)
        hFlush input
        ack <- exact output 1
        unless (ack == BS.singleton 1) (fail "native refusal lost BEGIN acknowledgement")
        let header = "TPNDX001" <> BS.replicate 32 1 <> BS.replicate 32 2
              <> BS.replicate 32 3 <> BS.replicate 32 4 <> put64 999999999999999999
            cumulative = mode == "cumulative"
            label = if cumulative then "native-budget" else "native-exchange"
            stage = if cumulative then 0 else 3
        BS.hPut input (BS.singleton 6 <> header <> frame "." <> put32 1 <> frame label)
        hFlush input
        observed <- exact output 161
        unless (observed == BS.singleton 4 <> header <> put64 stage <> put64 1)
          (fail "native refusal case did not reach its genuine outstanding demand")
        _ <- readFrame output
        let selectedHeader = if mode == "owner" then BS.take 40 header <> BS.singleton 9 <> BS.drop 41 header else header
            selectedStage = if mode == "stage" then stage + 1 else stage
            sequenceNumber = if mode == "sequence" then 2 else 1
            outcome = if mode == "outcome" then 2 else 0
            reply = if cumulative then BS.replicate (limit - 331 - 5) 114 else "first-product"
            lengthAndBody = if mode == "oversize" then put32 (fromIntegral limit + 1) else frame reply
        BS.hPut input (BS.singleton 5 <> selectedHeader <> put64 selectedStage <> put64 sequenceNumber
          <> BS.singleton outcome <> lengthAndBody)
        hFlush input
        terminal <- timeout 10000000 (waitForProcess child)
        unless (case terminal of Just (ExitFailure _) -> True; _ -> False)
          (fail ("malformed or exhausted native exchange did not retire: " ++ mode))
        emitted <- BS.hGet output 1
        unless (BS.null emitted) (fail "malformed native exchange emitted an unconfirmed terminal result")
        hClose input
        hClose output
  leftovers <- listDirectory scratch
  unless (null leftovers) (fail "malformed native exchange retained capture files")

-- The subprocess owns real request channels. No test constructor can issue an
-- exchange or borrow independently of the active handler.
checkSourceBorrowing :: FilePath -> IO ()
checkSourceBorrowing scratch = withSourceChild scratch $ \input output child -> do
  beginSource input "source-concurrent"
  payloads <- replicateM 3 $ do
    (stage,number,payload) <- readDemand output
    unless (stage == 3) (fail "concurrent SOURCE stage changed")
    replyDemand input stage number 0 "observed"
    pure (number,payload)
  unless (sort (map fst payloads) == [1,2,3]
      && sort (map snd payloads) == sort (map sourcePayload ["left","right","left"]))
    (fail "concurrent SOURCE frames were interleaved, lost, or duplicated")
  (stage,number,actualSource) <- readDemand output
  unless (stage == 3 && number == 4 && actualSource == dependencyPacket "TPDEPENDENCYQUERY1" 0)
    (fail "actual DependencyPlan SOURCE encoder was not accepted")
  replyDemand input stage number 0 (dependencyPacket "TPDEPENDENCYREPLY1" 0)
  sourceTerminal output
  beginSource input "source-stale"
  (currentStage,currentNumber,current) <- readDemand output
  unless (currentStage == 5 && currentNumber == 1 && current == sourcePayload "current")
    (fail "stale request/stage borrow wrote a frame")
  replyDemand input currentStage currentNumber 0 "observed"
  sourceTerminal output
  beginSource input "source-kind"
  (nativeStage,nativeNumber,actualNative) <- readDemand output
  unless (nativeStage == 0 && nativeNumber == 1 && actualNative == dependencyPacket "TPDEPENDENCYQUERY1" 1)
    (fail "actual DependencyPlan native encoder changed")
  replyDemand input nativeStage nativeNumber 0 (dependencyPacket "TPDEPENDENCYREPLY1" 1)
  (echoStage,echoNumber,echo) <- readDemand output
  unless (echoStage == 0 && echoNumber == 2 && echo == "request-native-encoding")
    (fail "native query echo control changed")
  replyDemand input echoStage echoNumber 0 actualNative
  sourceTerminal output
  beginSource input "source-semantic"
  forM_ [(1,"malformed-semantic-reply"),(2,dependencyPacket "TPDEPENDENCYREPLY1" 0)] $ \(expectedNumber,body) -> do
    (semanticStage,semanticNumber,query) <- readDemand output
    unless (semanticStage == 0 && semanticNumber == expectedNumber && query == dependencyPacket "TPDEPENDENCYQUERY1" 0)
      (fail "fully framed semantic refusal corrupted the transport")
    replyDemand input semanticStage semanticNumber 0 body
  sourceTerminal output
  BS.hPut input (BS.singleton 0)
  hFlush input
  ended <- exact output 1
  unless (ended == BS.singleton 1) (fail "SOURCE borrowing lost END")
  hClose input
  status <- waitForProcess child
  unless (status == ExitSuccess) (fail "SOURCE borrowing child did not exit cleanly")

-- Both valid and stale borrowers queue behind the actual outstanding exchange.
-- The next bytes must be the failed terminal: another demand is a failure.
checkSourceRefusal :: FilePath -> IO ()
checkSourceRefusal scratch = withSourceChild scratch $ \input output child -> do
  beginSource input "source-refusal"
  (stage,number,payload) <- readDemand output
  unless (stage == 3 && number == 1 && payload == dependencyPacket "TPDEPENDENCYQUERY1" 0)
    (fail "first refusal did not reach actual SOURCE demand")
  writeFile (scratch ++ "/refusal-frame") "observed"
  awaitFile (scratch ++ "/refusal-waiters")
  replyDemand input stage number 1 firstRefusal
  sourceFailedTerminal output "worker-failure" "NativeDemandRejected OriginalUnavailable"
  beginSource input "source-stale"
  (freshStage,freshNumber,freshPayload) <- readDemand output
  unless (freshStage == 5 && freshNumber == 1 && freshPayload == sourcePayload "current")
    (fail "refusal replay leaked state into the next request")
  replyDemand input freshStage freshNumber 0 "observed"
  sourceTerminal output
  beginSource input "refusal-malformed"
  (badStage,badNumber,_) <- readDemand output
  replyDemand input badStage badNumber 1 "malformed-semantic-refusal"
  sourceFailedTerminal output "worker-failure" "malformed refusal retained"
  finishSourceChild input output child
  removeFile (scratch ++ "/refusal-frame")
  removeFile (scratch ++ "/refusal-waiters")

checkRefusalTerminal :: FilePath -> IO ()
checkRefusalTerminal scratch = do
  -- Returned diagnostics carry presentation, while the typed completion owns
  -- success or failure. Neither returned severity may override a refusal.
  withSourceChild scratch $ \input output child -> do
    forRequest input output "diagnostic-error" $ \code out _ -> do
      assertReportMessages "success" [("error","typed-diagnostic-error")] out
      unless (code == 0) (fail "returned diagnostic severity changed completion authority")
    forRequest input output "diagnostic-warning" $ \code out _ -> do
      assertReportMessages "success" [("warning","typed-diagnostic-warning")] out
      unless (code == 0) (fail "DiagnosticResponse warnings did not preserve success")
    forM_ ["refusal-error","refusal-render-failure"] $ \mode -> do
      beginSource input mode
      (stage,number,_) <- readDemand output
      replyDemand input stage number 1 firstRefusal
      let (outcome,message) = if mode == "refusal-error"
            then ("source-failure","typed-source-rejection") else ("worker-failure","capture exceeds")
      sourceFailedTerminal output outcome message
    finishSourceChild input output child
  forM_ ["refusal-swallowed","refusal-returned-error","refusal-warning"] $ \mode ->
    withSourceChild scratch $ \input output child -> do
      beginSource input mode
      (stage,number,_) <- readDemand output
      replyDemand input stage number 1 firstRefusal
      finished <- timeout 10000000 (waitForProcess child)
      unless (case finished of Just (ExitFailure _) -> True; _ -> False)
        (fail "swallowed semantic refusal did not retire instead of succeeding")
      bytes <- BS.hGet output 1
      unless (BS.null bytes) (fail "swallowed semantic refusal emitted a terminal or retry")

sourceFailedTerminal :: Handle -> String -> String -> IO ()
sourceFailedTerminal output outcome message = do
  tag <- exact output 1
  code <- word32 <$> exact output 4
  out <- readFrame output
  _ <- readFrame output
  unless (tag == BS.singleton 0 && code == 1)
    (fail ("refused request lost its single failed terminal: " ++ show (tag,code,out)))
  assertReportMessages outcome [("error",message)] out

finishSourceChild :: Handle -> Handle -> ProcessHandle -> IO ()
finishSourceChild input output child = do
  BS.hPut input (BS.singleton 0)
  hFlush input
  ended <- exact output 1
  unless (ended == BS.singleton 1) (fail "semantic refusal lost clean END")
  hClose input
  status <- waitForProcess child
  unless (status == ExitSuccess) (fail "semantic refusal poisoned worker transaction")

checkSourceBorrowCancellation :: FilePath -> IO ()
checkSourceBorrowCancellation scratch = forM_ ["source-cancel","source-partial"] $ \mode ->
  withSourceChild scratch $ \input output child -> do
    beginSource input mode
    (_,_,payload) <- readDemand output
    unless (payload == sourcePayload (if mode == "source-cancel" then "blocked-read" else "partial"))
      (fail "SOURCE cancellation/partial case lost its initial full frame")
    if mode == "source-cancel"
      then writeFile (scratch ++ "/cancel-source") "cancel"
      else BS.hPut input (BS.singleton 5 <> BS.take 10 sourceHeader) >> hFlush input >> hClose input
    -- A blocked read and a blocked gate waiter must both settle; poison must
    -- prevent any retry or terminal frame after either partial wire history.
    finished <- timeout 10000000 (waitForProcess child)
    unless (case finished of Just (ExitFailure _) -> True; _ -> False)
      (fail ("SOURCE cancellation/partial frame did not retire within safeguard: " ++ BSC.unpack mode))
    emitted <- BS.hGet output 1
    unless (BS.null emitted) (fail "poisoned SOURCE borrow wrote retry or terminal bytes")

sourceHeader :: BS.ByteString
sourceHeader = "TPNDX001" <> BS.replicate 32 1 <> BS.replicate 32 2
  <> BS.replicate 32 3 <> BS.replicate 32 4 <> put64 999999999999999999

dependencyPacket :: BS.ByteString -> Word64 -> BS.ByteString
dependencyPacket tag origin = BS.singleton 0x83 <> cborText tag <> BS.pack [fromIntegral origin,0x80]

beginSource :: Handle -> BS.ByteString -> IO ()
beginSource input label = do
  BS.hPut input (BS.singleton 6 <> sourceHeader <> frame "." <> put32 1 <> frame label)
  hFlush input

readDemand :: Handle -> IO (Word64,Word64,BS.ByteString)
readDemand output = do
  header <- exact output 161
  unless (BS.take 145 header == BS.singleton 4 <> sourceHeader)
    (fail "SOURCE demand lost its exact request binding")
  payload <- readFrame output
  pure (number (BS.take 8 (BS.drop 145 header)),number (BS.drop 153 header),payload)
  where number = BS.foldr (\byte total -> fromIntegral byte .|. (total `shiftL` 8)) 0

replyDemand :: Handle -> Word64 -> Word64 -> Word8 -> BS.ByteString -> IO ()
replyDemand input stage number outcome body = do
  BS.hPut input (BS.singleton 5 <> sourceHeader <> put64 stage <> put64 number <> BS.singleton outcome <> frame body)
  hFlush input

sourceTerminal :: Handle -> IO ()
sourceTerminal output = do
  tag <- exact output 1
  code <- word32 <$> exact output 4
  out <- readFrame output
  err <- readFrame output
  unless (tag == BS.singleton 0 && code == 0 && out == successReport && BS.null err)
    (fail ("SOURCE exchange did not finish successfully: " ++ show (tag,code,out,err)))

withSourceChild :: FilePath -> (Handle -> Handle -> ProcessHandle -> IO a) -> IO a
withSourceChild scratch use = do
  executable <- lookupEnv "TIDEPOOL_TEST_WORKER_RESPONSE_CHILD" >>= maybe
    (fail "missing declared worker-response child executable") pure
  bracket (createProcess (proc executable ["--worker",scratch])
      {std_in=CreatePipe,std_out=CreatePipe,std_err=Inherit})
    (\(_,_,_,child) -> terminateProcess child >> void (waitForProcess child)) $
    \(inputHandle,outputHandle,_,child) -> do
      input <- maybe (fail "missing SOURCE stdin") pure inputHandle
      output <- maybe (fail "missing SOURCE stdout") pure outputHandle
      BS.hPut input (BS.singleton 1)
      hFlush input
      ack <- exact output 1
      unless (ack == BS.singleton 1) (fail "SOURCE child lost BEGIN")
      completed <- timeout 10000000 (use input output child)
      maybe (fail "SOURCE protocol control timed out") pure completed

checkPartialAcquisition :: FilePath -> IO ()
checkPartialAcquisition scratch = do
  executable <- lookupEnv "TIDEPOOL_TEST_WORKER_RESPONSE_CHILD" >>= maybe
    (fail "missing declared worker-response child executable") pure
  bracket (createProcess (proc executable ["--worker-one-fd-slot", scratch])
      { std_in = CreatePipe, std_out = CreatePipe, std_err = Inherit })
    (\(_, _, _, child) -> terminateProcess child >> void (waitForProcess child)) $
    \(inputHandle, outputHandle, _, child) -> do
      input <- maybe (fail "missing limited worker stdin pipe") pure inputHandle
      output <- maybe (fail "missing limited worker stdout pipe") pure outputHandle
      BS.hPut input (BS.singleton 1)
      hFlush input
      ack <- exact output 1
      unless (ack == BS.singleton 1) (fail "missing limited transaction acknowledgement")
      forRequest input output "partial-open" $ \code out _ ->
        unless (code == 1 && BSC.isInfixOf "worker-failure" out)
          (fail "partial capture acquisition did not deliver its single failure report")
      ackPartial <- timeout 10000000 (exact output 1)
      unless (ackPartial == Just (BS.singleton 1))
        (fail "second tempfile acquisition did not unwind the transaction")
      BS.hPut input (BS.singleton 1)
      hFlush input
      ackNext <- exact output 1
      unless (ackNext == BS.singleton 1) (fail "worker did not accept a later transaction")
      forRequest input output "next-request" $ \code out err ->
        unless (code == 0 && out == successReport && err == "complete-next-request")
          (fail "partial capture acquisition leaked resources")
      BS.hPut input (BS.singleton 0)
      hFlush input
      ackRecovered <- exact output 1
      unless (ackRecovered == BS.singleton 1) (fail "missing post-recovery acknowledgement")
      hClose input
      status <- waitForProcess child
      unless (status == ExitSuccess) (fail "worker failed after partial acquisition recovery")
      leftovers <- listDirectory scratch
      unless (null leftovers) (fail ("partial acquisition retained tempfiles: " ++ show leftovers))

checkSourceFreeCleanup :: FilePath -> IO ()
checkSourceFreeCleanup root = mapM_ one
  ["source-free-capture-failure", "source-free-rejected-capture-failure"]
  where
    one mode = do
      let scratch = root ++ "/" ++ mode
      createDirectory scratch
      checkSourceFreeCleanupAt mode scratch

checkSourceFreeCleanupAt :: String -> FilePath -> IO ()
checkSourceFreeCleanupAt mode scratch = do
  executable <- lookupEnv "TIDEPOOL_TEST_WORKER_RESPONSE_CHILD" >>= maybe
    (fail "missing declared worker-response child executable") pure
  bracket (createProcess (proc executable ["--worker", scratch])
      { std_in = CreatePipe, std_out = CreatePipe, std_err = Inherit })
    (\(_, _, _, child) -> terminateProcess child >> void (waitForProcess child)) $
    \(inputHandle, outputHandle, _, child) -> do
      input <- maybe (fail "missing source-free worker stdin pipe") pure inputHandle
      output <- maybe (fail "missing source-free worker stdout pipe") pure outputHandle
      BS.hPut input (BS.singleton 1)
      hFlush input
      ack <- exact output 1
      unless (ack == BS.singleton 1) (fail "missing source-free transaction acknowledgement")
      forRequest input output mode $ \code out _ -> do
        unless (code == 1 && BSC.isInfixOf "worker-failure" out
            && not (BSC.isInfixOf "TPEXO" out))
          (fail "capture cleanup failure exposed an unconfirmed Ready result")
        if mode == "source-free-rejected-capture-failure"
          then unless (BSC.isInfixOf "AcquisitionRejected" out)
            (fail "unsafe capture discarded the typed acquisition refusal")
          else pure ()
      -- Keep stdin live: EOF would independently terminate a worker that had
      -- incorrectly continued after its unsafe response cleanup.
      terminal <- timeout 10000000 (waitForProcess child)
      case terminal of
        Just (ExitFailure _) -> pure ()
        _ -> fail "unsafe source-free cleanup did not retire the worker"
      hClose input
      leftovers <- listDirectory scratch
      unless (null leftovers) (fail ("source-free capture retained tempfiles: " ++ show leftovers))

catchThreadKilled :: AsyncException -> IO ()
catchThreadKilled ThreadKilled = pure ()
catchThreadKilled failure = throwIO failure

catchResourceExhaustion :: IOException -> IO ()
catchResourceExhaustion failure
  | isFullError failure = pure ()
  | otherwise = throwIO failure

-- The worker child leaves exactly one descriptor slot available, so capture
-- opens its stdout file and fails while acquiring the stderr file.
withOneFreeFd :: IO a -> IO a
withOneFreeFd action = do
  ResourceLimits oldSoft hard <- getResourceLimit ResourceOpenFiles
  testSoft <- case hard of
    ResourceLimit hardMaximum -> pure (ResourceLimit (min 64 hardMaximum))
    ResourceLimitInfinity -> pure (ResourceLimit 64)
    ResourceLimitUnknown -> fail "cannot determine open-file hard limit for worker test"
  setResourceLimit ResourceOpenFiles (ResourceLimits testSoft hard)
  held <- fillUntilExhausted []
  action `finally` do
    mapM_ hClose held
    setResourceLimit ResourceOpenFiles (ResourceLimits oldSoft hard)
  where
    fillUntilExhausted held = do
      result <- tryDuplicate
      case result of
        Right handle -> fillUntilExhausted (handle : held)
        Left failure
          | isFullError failure -> case held of
              one : rest -> hClose one >> pure rest
              [] -> throwIO failure
          | otherwise -> throwIO failure
    tryDuplicate = do
      result <- try (hDuplicate stdin)
      case result of
        Left failure -> pure (Left (failure :: IOException))
        Right handle -> pure (Right handle)

forRequest :: Handle -> Handle -> String -> (Word32 -> BS.ByteString -> BS.ByteString -> IO ()) -> IO ()
forRequest input output label assertion = do
  BS.hPut input (BS.singleton 1 <> frame "." <> put32 1 <> frame (BSC.pack label))
  hFlush input
  received <- timeout 10000000 $ do
    code <- word32 <$> exact output 4
    out <- readFrame output
    err <- readFrame output
    assertion code out err
  maybe (fail "worker response timed out") pure received

exact :: Handle -> Int -> IO BS.ByteString
exact handle count = do
  bytes <- BS.hGet handle count
  unless (BS.length bytes == count) (fail "truncated test response")
  pure bytes

readFrame :: Handle -> IO BS.ByteString
readFrame handle = do
  count <- word32 <$> exact handle 4
  unless (fromIntegral count <= limit) (fail "unbounded test response")
  exact handle (fromIntegral count)

frame :: BS.ByteString -> BS.ByteString
frame bytes = put32 (fromIntegral (BS.length bytes)) <> bytes

put32 :: Word32 -> BS.ByteString
put32 value = BS.pack [fromIntegral (value `shiftR` n) | n <- [0, 8, 16, 24]]

put64 :: Word64 -> BS.ByteString
put64 value = BS.pack [fromIntegral (value `shiftR` n) | n <- [0,8..56]]

word32 :: BS.ByteString -> Word32
word32 bytes = foldr (.|.) 0 [fromIntegral (BS.index bytes i) `shiftL` (8 * i) | i <- [0..3]]

-- Real resident and bound compiler owners feed the same production transport.
-- Only the capture files in this child's private TMPDIR are faulted; no compiler
-- result, executor, or cleanup assessment is manufactured by the test.
compilerClusterChild :: FilePath -> IO ()
compilerClusterChild scratch = do
  let captures = scratch ++ "/captures"
      source = scratch ++ "/CompletionControl.hs"
  createDirectory captures
  setEnv "TMPDIR" captures
  originalCapabilities <- getNumCapabilities
  withResidentCompilerScopes [scratch] $ \runScope ->
    runWorkerLoop $ \transaction -> runScope (pure ()) $ \scope ->
      transaction $ \sink _ _ arguments -> do
        mode <- case arguments of [value] -> pure value; _ -> fail "invalid cluster control"
        let primaryFailure = mode `elem` ["source-refusal", "read-primary", "remove-primary"]
        writeFile source (if primaryFailure
          then "module CompletionControl where\nresult :: Int\nresult = True\n"
          else "module CompletionControl where\nresult :: Int\nresult = 41\n")
        request <- case workerRequestFromArgv (workerArgv [Input source, Include scratch, CheckSource]) of
          Right (Just value) -> pure value
          _ -> fail "invalid real compiler control request"
        completed <- flattenCompletion <$> captureCompletion
          (withBoundCompilerRequest sink scope (admitFixtureRequest request) $ \bound -> do
            body <- captureCompletion $ do
              _ <- compileBoundRequest bound CheckedEnvironment mempty GeneralCompile Nothing source
              if mode == "cancel" then throwIO ThreadKilled
                else pure [Diag Nothing DiagWarning "original-cluster-warning"]
            case mode of
              "read-success" -> removeCaptureFiles captures
              "read-primary" -> removeCaptureFiles captures
              "remove-success" -> setFileMode captures 0o500
              "remove-primary" -> setFileMode captures 0o500
              _ -> pure ()
            finishCompletion body)
        restoredCapabilities <- getNumCapabilities
        unless (restoredCapabilities == originalCapabilities)
          (fail "bound compiler did not restore its admitted capabilities")
        pure (fmap DiagnosticResponse completed)

removeCaptureFiles :: FilePath -> IO ()
removeCaptureFiles captures = do
  names <- listDirectory captures
  let owned = filter (\name -> "tidepool-worker-stdout" `isPrefixOf` name
                    || "tidepool-worker-stderr" `isPrefixOf` name) names
  unless (length owned == 2) (fail ("expected exactly two active capture files: " ++ show names))
  mapM_ (removeFile . ((captures ++ "/") ++)) owned

checkCompilerClusterReuse :: FilePath -> IO ()
checkCompilerClusterReuse scratch = withCompilerCluster scratch $ \input output child -> do
  forRequest input output "source-refusal" $ \code out _ -> do
    assertReport "source-failure" out
    unless (code == 1 && BSC.isInfixOf "source-failure" out && BSC.count '\n' out == 1)
      (fail "real source refusal did not produce exactly one source report")
  forRequest input output "healthy" $ \code out _ -> do
    assertReport "success" out
    unless (code == 0 && BSC.isInfixOf "original-cluster-warning" out
        && BSC.isInfixOf "success" out && BSC.count '\n' out == 1)
      (fail "clean source refusal prevented the next real request")
  BS.hPut input (BS.singleton 0)
  hFlush input
  ack <- exact output 1
  unless (ack == BS.singleton 1) (fail "clean cluster did not confirm transaction END")
  hClose input
  status <- waitForProcess child
  unless (status == ExitSuccess) (fail "clean compiler cluster did not retire")
  assertNoCaptureFiles scratch

checkCompilerClusterCleanup :: FilePath -> IO ()
checkCompilerClusterCleanup scratch = mapM_ one
  ["read-success", "read-primary", "remove-success", "remove-primary"]
  where
    one mode = do
      let directory = scratch ++ "/" ++ mode
      createDirectory directory
      (withCompilerCluster directory $ \input output child -> do
        forRequest input output mode $ \code out _ -> do
          assertReport "worker-failure" out
          unless (code == 1 && BSC.isInfixOf "worker-failure" out
              && BSC.count '\n' out == 1)
            (fail "unsafe real capture did not deliver one worker-failure report")
          let originals = if mode `elem` ["read-primary", "remove-primary"]
                then ["CompletionControl.hs", "Int", "Bool"] else ["original-cluster-warning"]
          unless (all (`BSC.isInfixOf` out) originals)
            (fail "capture failure erased the original body outcome")
          unless (BSC.isInfixOf (if take 4 mode == "read" then "CaptureRead" else "CaptureRemove") out)
            (fail "capture failure lost its exact cleanup stage")
        ended <- timeout 10000000 (BS.hGet output 1)
        unless (ended == Just BS.empty)
          (fail "unsafe capture acknowledged END or accepted another request")
        status <- timeout 10000000 (waitForProcess child)
        unless (case status of Just (ExitFailure _) -> True; _ -> False)
          (fail "unsafe capture did not physically retire its worker"))
        `finally` setFileMode (directory ++ "/captures") 0o700

checkCompilerClusterCancellation :: FilePath -> IO ()
checkCompilerClusterCancellation scratch = withCompilerCluster scratch $ \input output child -> do
  BS.hPut input (BS.singleton 1 <> frame "." <> put32 1 <> frame "cancel")
  hFlush input
  ended <- timeout 10000000 (BS.hGet output 1)
  unless (ended == Just BS.empty) (fail "cancellation became a report or END acknowledgement")
  status <- timeout 10000000 (waitForProcess child)
  unless (case status of Just (ExitFailure _) -> True; _ -> False)
    (fail "cancelled compiler cluster did not retire")
  assertNoCaptureFiles scratch

withCompilerCluster :: FilePath -> (Handle -> Handle -> ProcessHandle -> IO a) -> IO a
withCompilerCluster scratch use = do
  started <- getMonotonicTimeNSec
  executable <- lookupEnv "TIDEPOOL_TEST_WORKER_RESPONSE_CHILD" >>= maybe
    (fail "missing declared compiler-cluster child") pure
  bracket (createProcess (proc executable ["--compiler-cluster", scratch])
      { std_in = CreatePipe, std_out = CreatePipe, std_err = Inherit })
    (\(_, _, _, child) -> terminateProcess child >> void (waitForProcess child)) $
    \(inputHandle, outputHandle, _, child) -> do
      input <- maybe (fail "missing compiler-cluster stdin") pure inputHandle
      output <- maybe (fail "missing compiler-cluster stdout") pure outputHandle
      BS.hPut input (BS.singleton 1)
      hFlush input
      -- A bounded control safeguard, not a production startup deadline or
      -- evidence that cold GHC setup fits it. Retain actual startup separately.
      ack <- timeout 10000000 (exact output 1)
      finished <- getMonotonicTimeNSec
      BSC.hPutStrLn stderr (BSC.pack ("compiler-cluster BEGIN elapsed-ns=" ++ show (finished - started)))
      unless (ack == Just (BS.singleton 1)) (fail "real compiler cluster did not admit BEGIN within control budget")
      use input output child

assertNoCaptureFiles :: FilePath -> IO ()
assertNoCaptureFiles scratch = do
  names <- listDirectory (scratch ++ "/captures")
  unless (all (not . isPrefixOf "tidepool-worker-") names)
    (fail "real compiler cluster retained a capture file")

-- Independent JSON parsing checks the report outcome and diagnostic fields,
-- and refuses concatenated terminal reports or malformed data.
assertReport :: String -> BS.ByteString -> IO ()
assertReport expected = assertReportMessages expected []

assertReportMessages :: String -> [(String,String)] -> BS.ByteString -> IO ()
assertReportMessages expected messages bytes = do
  python <- lookupEnv "TIDEPOOL_TEST_PYTHON" >>= maybe
    (fail "missing declared JSON verifier") pure
  (status, _, diagnostic) <- readProcessWithExitCode python
    (["-c", "import json,sys; r=json.loads(bytes(json.loads(sys.stdin.read())).decode('utf-8')); assert r['version']==2; assert r['outcome']==sys.argv[1]; assert isinstance(r['diagnostics'],list); assert all(any(d['severity']==severity and message in d['message'] for d in r['diagnostics']) for severity,message in zip(sys.argv[2::2],sys.argv[3::2]))", expected]
      ++ concatMap (\(severity,message) -> [severity,message]) messages)
    (show (BS.unpack bytes))
  unless (status == ExitSuccess) (fail ("terminal report is not one complete document: " ++ diagnostic))

-- Fixture requests pass the production pure admission before compiler binding.
admitFixtureRequest :: WorkerRequest -> AdmittedWorkerRequest
admitFixtureRequest request = either (error . show) id (admitWorkerRequest request)
