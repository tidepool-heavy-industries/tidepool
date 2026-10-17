{-# LANGUAGE ScopedTypeVariables #-}

-- | Framed request completion. Rust owns processes, deadlines and retirement;
-- this owner retains the protocol channels independently of redirected globals.
module Tidepool.WorkerServer (RequestHandler, runWorkerLoop, runDirectRequest) where

import Control.Exception (SomeException, mask, throwIO, try)
import Control.Monad (replicateM, unless)
import Data.Bits ((.|.), shiftL, shiftR)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BSC
import qualified Data.ByteString.Lazy as LBS
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.Text.Encoding.Error as TEE
import qualified Data.Text.Lazy as LT
import qualified Data.Text.Lazy.Encoding as LTE
import Data.Word (Word32)
import System.Directory (getTemporaryDirectory, removeFile)
import System.Exit (ExitCode(..))
import System.IO
  ( Handle, IOMode(ReadMode), hClose, hFileSize, hFlush, openTempFile
  , openBinaryFile, stderr, stdin, stdout )
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), ReportOutcome(..), renderDiagsJson)
import Tidepool.RequestCompletion
import Tidepool.WorkerDiagnostics (CaptureAssessment(..), renderCompletion)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)

type RequestHandler = PhaseSink -> FilePath -> [String] -> IO (Completion [Diag])

data OutputChannels = OutputChannels Handle Handle

data CaptureOutcome
  = Captured BS.ByteString BS.ByteString
  | RejectedCapture String
  | UnavailableCapture

withOutputChannels :: (OutputChannels -> IO a) -> IO a
withOutputChannels use = do
  completed <- withCompletionRelease CaptureClose (hDuplicate stdout) hClose $ \out ->
    withCompletionRelease CaptureClose (hDuplicate stderr) hClose $ \err ->
      captureCompletion (use (OutputChannels out err))
  case completionAsync completed of
    Just failure -> throwIO failure
    Nothing -> finishCompletion completed

-- The callback's GHC scope must release before END is acknowledged. Unsafe
-- request cleanup sends one diagnostic response, then unwinds that same scope.
runWorkerLoop :: ((RequestHandler -> IO ()) -> IO ()) -> IO ()
runWorkerLoop withTransaction = withOutputChannels $ \channels@(OutputChannels protocol _) ->
  let go = do
        command <- BS.hGet stdin 1
        if BS.null command then pure () else case BS.head command of
          1 -> do
            withTransaction (serveTransaction channels)
            BS.hPut protocol (BS.singleton 1)
            hFlush protocol
            go
          other -> throwIO (userError ("worker: unknown transaction command " ++ show other))
  in go
  where
    serveTransaction channels@(OutputChannels protocol _) handler = do
      BS.hPut protocol (BS.singleton 1)
      hFlush protocol
      requests channels handler
    requests channels@(OutputChannels protocol _) handler = do
      command <- hGetExactly stdin 1
      case BS.head command of
        0 -> pure ()
        1 -> do
          cwd <- decodeText <$> hGetFrame stdin
          argc <- getU32LE <$> hGetExactly stdin 4
          argv <- replicateM (fromIntegral argc) (decodeText <$> hGetFrame stdin)
          (completed, captured) <- captureOutput channels (handler (phaseSink channels) cwd argv)
          (code, out, err) <- renderCaptured channels completed captured
          emitPhase (phaseSink channels) WritingResponse
          BS.hPut protocol (encodeResponse (exitCodeToInt code) out err)
          hFlush protocol
          emitPhase (phaseSink channels) ResponseWritten
          unless (null (completionFailures completed)) $
            finishCompletion completed >> pure ()
          case captured of
            UnavailableCapture -> finishCompletion completed >> fail "worker capture unavailable"
            _ -> requests channels handler
        other -> throwIO (userError ("worker: unknown request command " ++ show other))

-- Direct CLI shares capture, release assessment and terminal rendering with the
-- framed owner. Main's callback includes its outer compiler-scope release.
runDirectRequest :: (PhaseSink -> IO (Completion [Diag])) -> IO ExitCode
runDirectRequest action = withOutputChannels $ \channels@(OutputChannels out err) -> do
  (completed, captured) <- captureOutput channels (action (phaseSink channels))
  (code, machine, human) <- renderCaptured channels completed captured
  emitPhase (phaseSink channels) WritingResponse
  BS.hPut out machine
  BS.hPut err human
  hFlush out
  hFlush err
  emitPhase (phaseSink channels) ResponseWritten
  pure code

phaseSink :: OutputChannels -> PhaseSink
phaseSink (OutputChannels _ diagnostics) phase = do
  BSC.hPutStrLn diagnostics (BSC.pack ("tidepool-request-phase-v1 " ++ show phase))
  hFlush diagnostics

hGetFrame :: Handle -> IO BS.ByteString
hGetFrame handle = do
  len <- getU32LE <$> hGetExactly handle 4
  hGetExactly handle (fromIntegral len)

hGetExactly :: Handle -> Int -> IO BS.ByteString
hGetExactly handle count = go count []
  where
    go 0 chunks = pure (BS.concat (reverse chunks))
    go remaining chunks = do
      bytes <- BS.hGet handle remaining
      if BS.null bytes
        then throwIO (userError "worker: truncated frame")
        else go (remaining - BS.length bytes) (bytes : chunks)

putU32LE :: Word32 -> BS.ByteString
putU32LE value = BS.pack
  [ fromIntegral (value `shiftR` 0)
  , fromIntegral (value `shiftR` 8)
  , fromIntegral (value `shiftR` 16)
  , fromIntegral (value `shiftR` 24)
  ]

getU32LE :: BS.ByteString -> Word32
getU32LE bytes =
      fromIntegral (BS.index bytes 0)
  .|. (fromIntegral (BS.index bytes 1) `shiftL` 8)
  .|. (fromIntegral (BS.index bytes 2) `shiftL` 16)
  .|. (fromIntegral (BS.index bytes 3) `shiftL` 24)

frame :: BS.ByteString -> BS.ByteString
frame bytes = putU32LE (fromIntegral (BS.length bytes)) <> bytes

encodeResponse :: Int -> BS.ByteString -> BS.ByteString -> BS.ByteString
encodeResponse code out err =
  putU32LE (fromIntegral code) <> frame out <> frame err

decodeText :: BS.ByteString -> String
decodeText = T.unpack . TE.decodeUtf8With TEE.lenientDecode

captureOutput :: OutputChannels -> IO (Completion [Diag]) -> IO (Completion [Diag], CaptureOutcome)
captureOutput channels action = mask $ \restore -> do
  opened <- try $ do
    directory <- getTemporaryDirectory
    openTempFile directory "tidepool-worker-stdout.txt"
  case opened of
    Left (failure :: SomeException) -> do
      completed <- captureCompletion (throwIO failure)
      pure (completed, UnavailableCapture)
    Right (outPath, outHandle) -> do
      openedErr <- try $ do
        directory <- getTemporaryDirectory
        openTempFile directory "tidepool-worker-stderr.txt"
      case openedErr of
        Left (failure :: SomeException) -> do
          completed <- captureCompletion (throwIO failure)
          failures <- attemptReleases [(CaptureClose, hClose outHandle), (CaptureRemove, removeFile outPath)]
          pure (appendCleanupFailures completed failures, UnavailableCapture)
        Right (errPath, errHandle) -> do
          completed <- flattenCompletion <$> captureCompletion (restore $ do
            hDuplicateTo outHandle stdout
            hDuplicateTo errHandle stderr
            action)
          restored <- observedRelease channels OutputRestore (restoreOutput channels)
          closed <- observedRelease channels CaptureClose $
            attemptReleases [(CaptureClose, hClose outHandle), (CaptureClose, hClose errHandle)]
          readStart <- attemptReleases
            [(CaptureRead, emitPhase (phaseSink channels) (CleanupStarted CaptureRead))]
          readResult <- readCapturedResponse outPath errPath
          readEnd <- attemptReleases
            [(CaptureRead, emitPhase (phaseSink channels) (CleanupFinished CaptureRead))]
          removed <- observedRelease channels CaptureRemove $
            attemptReleases [(CaptureRemove, removeFile outPath), (CaptureRemove, removeFile errPath)]
          let failures = restored ++ closed ++ readStart ++ asCleanupFailures CaptureRead readResult ++ readEnd ++ removed
              captured = case completionResult readResult of
                BodySucceeded (Right (out, err)) -> Captured out err
                BodySucceeded (Left reason) -> RejectedCapture reason
                _ -> UnavailableCapture
          pure (appendCleanupFailures completed failures, captured)

-- Observation is part of this release assessment, but cannot suppress any
-- actual release operation, including when its diagnostic channel is canceled.
observedRelease :: OutputChannels -> CleanupStage -> IO [CleanupFailure] -> IO [CleanupFailure]
observedRelease channels stage action = do
  started <- attemptReleases [(stage, emitPhase (phaseSink channels) (CleanupStarted stage))]
  completed <- captureCompletion action
  finished <- attemptReleases [(stage, emitPhase (phaseSink channels) (CleanupFinished stage))]
  pure (started ++ case completionResult completed of
    BodySucceeded failures -> failures ++ completionFailures completed ++ finished
    _ -> asCleanupFailures stage completed ++ finished)

restoreOutput :: OutputChannels -> IO [CleanupFailure]
restoreOutput (OutputChannels out err) = attemptReleases
  [ (OutputRestore, hFlush stdout), (OutputRestore, hFlush stderr)
  , (OutputRestore, hDuplicateTo out stdout), (OutputRestore, hDuplicateTo err stderr)
  ]

renderCaptured :: OutputChannels -> Completion [Diag] -> CaptureOutcome -> IO (ExitCode, BS.ByteString, BS.ByteString)
renderCaptured channels completed captured = do
  case completionAsync completed of
    Just failure -> throwIO failure
    Nothing -> pure ()
  emitPhase (phaseSink channels) RenderingResponse
  let assessment = case captured of
        Captured _ _ -> CaptureComplete
        RejectedCapture reason -> CaptureBudgetRejected reason
        UnavailableCapture -> CaptureUnavailable
      (capturedOut, capturedErr) = case captured of
        Captured out err -> (out, err)
        _ -> (BS.empty, BS.empty)
      remaining = maxResponsePayloadBytes
        - toInteger (BS.length capturedOut) - toInteger (BS.length capturedErr)
  (code, machine, human) <- renderCompletion completed assessment
  case boundedUtf8 remaining machine of
    Just out -> case boundedUtf8 (remaining - toInteger (BS.length out)) human of
      Just debug -> pure (code, out, capturedOut <> capturedErr <> debug)
      Nothing -> budgetFailure
    Nothing -> budgetFailure
  where
    budgetFailure = pure (ExitFailure 1, TE.encodeUtf8 (T.pack (renderDiagsJson ReportWorkerFailure
      [Diag Nothing DiagError ("compiler response diagnostics exceed "
        ++ show maxResponsePayloadBytes ++ " bytes")] ++ "\n")), BS.empty)

-- Force only the remaining response budget and one growth sentinel while
-- encoding. Do not concatenate or strictly encode bytes before their check.
boundedUtf8 :: Integer -> String -> Maybe BS.ByteString
boundedUtf8 remaining value
  | remaining < 0 = Nothing
  | otherwise =
      let bytes = LBS.toStrict (LBS.take (fromInteger remaining + 1)
            (LTE.encodeUtf8 (LT.pack value)))
      in if toInteger (BS.length bytes) > remaining then Nothing else Just bytes

-- Shared with the Rust response decoder. Prepared/native products remain files;
-- exceeding this diagnostic budget is infrastructure failure, never truncation.
maxResponsePayloadBytes :: Integer
maxResponsePayloadBytes = 16777216

readCapturedResponse :: FilePath -> FilePath -> IO (Completion (Either String (BS.ByteString, BS.ByteString)))
readCapturedResponse outPath errPath =
  withCompletionRelease CaptureClose (openBinaryFile outPath ReadMode) hClose $ \outHandle ->
  withCompletionRelease CaptureClose (openBinaryFile errPath ReadMode) hClose $ \errHandle ->
  captureCompletion $ do
    outSize <- hFileSize outHandle
    errSize <- hFileSize errHandle
    if outSize + errSize > maxResponsePayloadBytes
      then pure (Left ("compiler response capture exceeds " ++ show maxResponsePayloadBytes
        ++ " bytes (stdout=" ++ show outSize ++ ", stderr=" ++ show errSize ++ ")"))
      else do
        -- Read only the measured lengths plus one growth sentinel per file.
        -- A concurrent inherited writer cannot force unbounded allocation or
        -- turn a partial captured response into successful typed data.
        out <- BS.hGet outHandle (fromInteger outSize)
        outExtra <- BS.hGet outHandle 1
        err <- BS.hGet errHandle (fromInteger errSize)
        errExtra <- BS.hGet errHandle 1
        finalOutSize <- hFileSize outHandle
        finalErrSize <- hFileSize errHandle
        if toInteger (BS.length out) /= outSize || not (BS.null outExtra)
            || toInteger (BS.length err) /= errSize || not (BS.null errExtra)
            || finalOutSize /= outSize || finalErrSize /= errSize
          then pure (Left "compiler response capture changed while being read")
          else pure (Right (out, err))

exitCodeToInt :: ExitCode -> Int
exitCodeToInt ExitSuccess = 0
exitCodeToInt (ExitFailure code) = code
