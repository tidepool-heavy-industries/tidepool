{-# LANGUAGE ScopedTypeVariables #-}

module WorkerResponseCases where

import Control.Exception (AsyncException(..), IOException, bracket, catch, finally, throwIO, try)
import Control.Monad (unless, void)
import Data.Bits ((.|.), shiftL, shiftR)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BSC
import Data.Word (Word32)
import Data.List (isPrefixOf)
import Data.IORef (newIORef, atomicModifyIORef')
import System.Directory (createDirectory, getTemporaryDirectory, listDirectory, removeFile, removeDirectoryRecursive)
import System.Environment (lookupEnv, setEnv)
import System.Exit (ExitCode(..))
import System.IO (Handle, hClose, hFlush, openTempFile, stdin, stdout, stderr)
import System.Process (ProcessHandle, CreateProcess(..), StdStream(..), createProcess, proc, terminateProcess, waitForProcess, readProcessWithExitCode)
import System.IO.Error (isFullError)
import System.Timeout (timeout)
import System.Posix.Files (setFileMode)
import GHC.Conc (getNumCapabilities)
import GHC.Clock (getMonotonicTimeNSec)
import Tidepool.CompilerRequest (withBoundCompilerRequest, compileBoundRequest)
import Tidepool.DiagJson (Diag(..), DiagSeverity(..))
import Tidepool.ExtractRequest (workerRequestFromArgv, workerArgv, RequestField(..))
import Tidepool.GhcPipeline
  ( withResidentCompilerScopes, PipelineSelection(CheckedEnvironment), CompilePurpose(GeneralCompile) )
import System.Posix.Resource
  ( Resource(..), ResourceLimit(..), ResourceLimits(..), getResourceLimit, setResourceLimit )
import GHC.IO.Handle (hDuplicate)
import Tidepool.WorkerServer (runWorkerLoop)
import Tidepool.RequestCompletion
  ( captureCompletion, flattenCompletion, finishCompletion )

limit :: Int
limit = 16777216

successReport :: BS.ByteString
successReport = BSC.pack "{\"version\":2,\"outcome\":\"success\",\"diagnostics\":[]}\n"

workerChild :: Bool -> FilePath -> IO ()
workerChild oneFreeFd scratch = do
  setEnv "TMPDIR" scratch
  if oneFreeFd
    then do
      first <- newIORef True
      runWorkerLoop $ \transaction -> do
        limited <- atomicModifyIORef' first (\isFirst -> (False, isFirst))
        if limited
          then withOneFreeFd (transaction handler `catch` catchResourceExhaustion)
          else transaction handler `catch` catchThreadKilled
    else runWorkerLoop (\transaction -> transaction handler `catch` catchThreadKilled)
  where
    handler _ _ args = captureCompletion $ do
      case args of
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
      pure []

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
      transaction $ \sink _ arguments -> do
        mode <- case arguments of [value] -> pure value; _ -> fail "invalid cluster control"
        let primaryFailure = mode `elem` ["source-refusal", "read-primary", "remove-primary"]
        writeFile source (if primaryFailure
          then "module CompletionControl where\nresult :: Int\nresult = True\n"
          else "module CompletionControl where\nresult :: Int\nresult = 41\n")
        request <- case workerRequestFromArgv (workerArgv [Input source, Include scratch, CheckSource]) of
          Right (Just value) -> pure value
          _ -> fail "invalid real compiler control request"
        completed <- flattenCompletion <$> captureCompletion
          (withBoundCompilerRequest sink scope request $ \bound -> do
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
        pure completed

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

-- Independent JSON parsing refuses concatenated terminal reports and malformed
-- data; rendered marker checks above then assess the authored semantic facts.
assertReport :: String -> BS.ByteString -> IO ()
assertReport expected bytes = do
  python <- lookupEnv "TIDEPOOL_TEST_PYTHON" >>= maybe
    (fail "missing declared JSON verifier") pure
  (status, _, diagnostic) <- readProcessWithExitCode python
    ["-c", "import json,sys; r=json.loads(bytes(json.loads(sys.stdin.read())).decode('utf-8')); assert r['version']==2; assert r['outcome']==sys.argv[1]; assert isinstance(r['diagnostics'],list)", expected]
    (show (BS.unpack bytes))
  unless (status == ExitSuccess) (fail ("terminal report is not one complete document: " ++ diagnostic))
