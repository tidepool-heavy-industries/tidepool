{-# LANGUAGE ScopedTypeVariables #-}

-- | Framed request completion. Rust owns processes, deadlines and retirement;
-- this owner retains the protocol channels independently of redirected globals.
module Tidepool.WorkerServer
  ( WorkerResponse(..), RequestHandler, runWorkerLoop, runDirectRequest
  , NativeDemandExchange, NativeDemandReply(..), exchangeNativeDemand, requireNativeDemandOwner
  , SourceDemandBorrow, borrowSourceDemand, exchangeSourceDemand
  ) where

import Control.Exception (Exception, SomeException, mask, throwIO, try, finally, onException)
import Control.Concurrent (ThreadId, myThreadId, MVar, newMVar, withMVar)
import qualified Codec.CBOR.Decoding as D
import Codec.CBOR.Read (deserialiseFromBytes)
import qualified Data.ByteString.Lazy as BL
import Data.IORef (IORef, newIORef, readIORef, writeIORef)
import Control.Monad (replicateM, unless, when)
import Data.Bits ((.|.), shiftL, shiftR)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BSC
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.Text.Encoding.Error as TEE
import Data.Word (Word32, Word64)
import System.Directory (getTemporaryDirectory, removeFile)
import System.Exit (ExitCode(..))
import System.IO
  ( Handle, IOMode(ReadMode), hClose, hFileSize, hFlush, openTempFile
  , openBinaryFile, stderr, stdin, stdout )
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), ReportOutcome(..), renderDiagsJson)
import Tidepool.ExtractRequest (EnsureExactInputsOutcome(..), encodeEnsureExactInputsOutcomeWithin)
import Tidepool.ExtractUtil (boundedUtf8)
import Tidepool.RequestCompletion
import Tidepool.WorkerDiagnostics (CaptureAssessment(..), renderCompletion)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)

data WorkerResponse
  = DiagnosticResponse [Diag]
  | ExactInputsResponse EnsureExactInputsOutcome

type RequestHandler = PhaseSink -> Maybe NativeDemandExchange -> FilePath -> [String]
  -> IO (Completion WorkerResponse)

-- The active handler owns these channels and state. Correlation bytes grant no
-- compiler authority; the bound request checks them against its actual inputs.
data NativeDemandExchange = NativeDemandExchange !BS.ByteString !ThreadId !Handle !Handle
  !(IORef NativeRequestState) !(MVar ())

-- Only the wire gate reads or changes request state. Accounting survives every
-- transition, including interrupted transport; exception cleanup never restores
-- a prior Ready state.
data NativeRequestState
  = Ready !NativeAccounting
  | SemanticallyRefused !NativeAccounting !BS.ByteString
  | TransportBroken !NativeAccounting
  | Closed !NativeAccounting

data NativeAccounting = NativeAccounting
  { demandSequence :: !Word64
  , demandStage :: !(Maybe Word64)
  , demandSpent :: !Integer
  , sourceStage :: !(Maybe Word64)
  }

-- Issued only on the request handler. Compiler tasks borrow SOURCE observation
-- at this fixed stage; executable acquisition still requires the handler.
data SourceDemandBorrow = SourceDemandBorrow !NativeDemandExchange !Word64

data NativeDemandReply = NativeResolved BS.ByteString | NativeRefused BS.ByteString

data NativeDemandAccessFailure
  = NativeDemandClosed
  | NativeDemandWrongThread !ThreadId !ThreadId
  | NativeDemandSuccessfulTerminalAfterRefusal
  deriving (Show)

instance Exception NativeDemandAccessFailure

requestAccounting :: NativeRequestState -> NativeAccounting
requestAccounting state = case state of
  Ready accounting -> accounting
  SemanticallyRefused accounting _ -> accounting
  TransportBroken accounting -> accounting
  Closed accounting -> accounting

requireNativeHandler :: NativeDemandExchange -> IO ()
requireNativeHandler (NativeDemandExchange _ issuing _ _ _ _) = do
  actual <- myThreadId
  unless (actual == issuing) (throwIO (NativeDemandWrongThread issuing actual))

requireNativeActive :: NativeRequestState -> IO NativeAccounting
requireNativeActive state = case state of
  Ready accounting -> pure accounting
  SemanticallyRefused accounting _ -> pure accounting
  _ -> throwIO NativeDemandClosed

requireNativeDemandOwner :: NativeDemandExchange -> BS.ByteString -> BS.ByteString -> BS.ByteString -> IO ()
requireNativeDemandOwner exchange@(NativeDemandExchange header _ _ _ state gate) admission world inputs =
  withMVar gate $ \_ -> do
    requireNativeHandler exchange
    _ <- readIORef state >>= requireNativeActive
    unless (BS.length admission == 32 && BS.length world == 32 && BS.length inputs == 32
        && BS.take 96 (BS.drop 40 header) == admission <> world <> inputs) $
      fail "worker: native demand owner differs from admitted World inputs"

exchangeNativeDemand :: NativeDemandExchange -> Word64 -> BS.ByteString -> IO NativeDemandReply
exchangeNativeDemand exchange@(NativeDemandExchange _ _ _ _ _ gate) stage payload =
  withMVar gate $ \_ -> do
    requireNativeHandler exchange
    exchangeDemandLocked exchange stage payload

borrowSourceDemand :: NativeDemandExchange -> Word64 -> IO SourceDemandBorrow
borrowSourceDemand exchange@(NativeDemandExchange _ _ _ _ state gate) stage =
  withMVar gate $ \_ -> do
    requireNativeHandler exchange
    current <- readIORef state
    accounting <- requireNativeActive current
    when (maybe False (> stage) (sourceStage accounting)
        || maybe False (> stage) (demandStage accounting)) $
      fail "worker: SOURCE borrow stage regressed"
    let borrowed = accounting { sourceStage = Just stage }
    writeIORef state $ case current of
      SemanticallyRefused _ reply -> SemanticallyRefused borrowed reply
      _ -> Ready borrowed
    pure (SourceDemandBorrow exchange stage)

exchangeSourceDemand :: SourceDemandBorrow -> BS.ByteString -> IO NativeDemandReply
exchangeSourceDemand (SourceDemandBorrow exchange@(NativeDemandExchange _ _ _ _ state gate) stage) payload =
  withMVar gate $ \_ -> do
    current <- readIORef state
    accounting <- requireNativeActive current
    unless (sourceStage accounting == Just stage) (throwIO NativeDemandClosed)
    -- Even after refusal the borrowed capability cannot acquire native inputs.
    -- Full semantic decoding remains with the existing dependency owner.
    case deserialiseFromBytes sourceEnvelope (BL.fromStrict (BS.take 64 payload)) of
      Left _ -> fail "worker: SOURCE borrow requires a SOURCE dependency query"
      Right _ -> pure ()
    exchangeDemandLocked exchange stage payload
  where
    sourceEnvelope = do
      count <- D.decodeListLen
      label <- D.decodeString
      origin <- D.decodeWord64
      unless (count == 3 && label == T.pack "TPDEPENDENCYQUERY1" && origin == 0)
        (fail "not a SOURCE query")

-- Callers authorize before entering here. The gate covers validation, local
-- refusal replay, reservation, complete frame exchange and commit.
exchangeDemandLocked :: NativeDemandExchange -> Word64 -> BS.ByteString -> IO NativeDemandReply
exchangeDemandLocked (NativeDemandExchange header _ input protocol state _) stage payload = mask $ \restore -> do
  current <- readIORef state
  accounting <- requireNativeActive current
  when (maybe False (> stage) (demandStage accounting)) $
    fail "worker: native demand stage regressed"
  case current of
    SemanticallyRefused _ reply -> do
      writeIORef state (SemanticallyRefused (accounting { demandStage = Just stage }) reply)
      pure (NativeRefused reply)
    _ -> do
      let charged = demandSpent accounting + 331 + toInteger (BS.length payload)
      when (demandSequence accounting == maxBound || charged > maxResponsePayloadBytes) $
        fail "worker: invalid or over-budget native demand"
      let number = demandSequence accounting + 1
          reserved = accounting { demandSequence = number, demandStage = Just stage, demandSpent = charged }
          remaining = maxResponsePayloadBytes - charged
      -- Reserve before any interruptible IO; every exception below poisons this
      -- exact reservation before a queued borrower can acquire the gate.
      writeIORef state (Ready reserved)
      (outcome, count, body) <- restore (do
        BS.hPut protocol (BS.singleton 4 <> header <> putU64LE stage <> putU64LE number <> frame payload)
        hFlush protocol
        replyHeader <- hGetExactly input 162
        unless (BS.index replyHeader 0 == 5 && BS.take 144 (BS.drop 1 replyHeader) == header
            && getU64LE (BS.take 8 (BS.drop 145 replyHeader)) == stage
            && getU64LE (BS.take 8 (BS.drop 153 replyHeader)) == number) $
          fail "worker: native reply does not match the outstanding demand"
        let outcome = BS.index replyHeader 161
        unless (outcome == 0 || outcome == 1) (fail "worker: invalid native reply outcome")
        count <- getU32LE <$> hGetExactly input 4
        when (toInteger count > remaining) (fail "worker: native reply exceeds response budget")
        body <- hGetExactly input (fromIntegral count)
        pure (outcome, count, body))
        `onException` writeIORef state (TransportBroken reserved)
      let committed = reserved { demandSpent = charged + toInteger count }
      writeIORef state (if outcome == 0 then Ready committed else SemanticallyRefused committed body)
      pure (if outcome == 0 then NativeResolved body else NativeRefused body)

-- Capture cleanup must still close/remove its files after malformed exchange
-- framing poisoned the channel. This observation does not authorize a response.
nativeCaptureBudget :: NativeDemandExchange -> IO Integer
nativeCaptureBudget (NativeDemandExchange _ _ _ _ state gate) = withMVar gate $ \_ -> do
  accounting <- requestAccounting <$> readIORef state
  pure (max 0 (maxResponsePayloadBytes - demandSpent accounting))

-- Compiler tasks have settled before borrowing is revoked. Render first, then
-- admit the final ExitCode; a successful body can still render as a failure.
withNativeTerminal :: Maybe NativeDemandExchange -> IO ()
  -> (Integer -> IO (ExitCode, BS.ByteString, BS.ByteString))
  -> ((ExitCode, BS.ByteString, BS.ByteString) -> IO a) -> IO a
withNativeTerminal Nothing _ render write = render maxResponsePayloadBytes >>= write
withNativeTerminal (Just exchange@(NativeDemandExchange _ _ _ _ state gate)) poisoned render write =
  withMVar gate $ \_ -> mask $ \restore -> do
    requireNativeHandler exchange
    current <- readIORef state
    let accounting = (requestAccounting current) { sourceStage = Nothing }
        close = do
          settled <- readIORef state
          case settled of
            TransportBroken _ -> pure ()
            _ -> writeIORef state (Closed accounting)
    case current of
      TransportBroken _ -> poisoned >> throwIO NativeDemandClosed
      Closed _ -> throwIO NativeDemandClosed
      _ -> do
        writeIORef state $ case current of
          SemanticallyRefused _ reply -> SemanticallyRefused accounting reply
          _ -> Ready accounting
        (do
          rendered@(code,_,_) <- restore (render (max 0 (maxResponsePayloadBytes - demandSpent accounting)))
          case current of
            SemanticallyRefused _ _ | code == ExitSuccess ->
              throwIO NativeDemandSuccessfulTerminalAfterRefusal
            _ -> pure ()
          restore (write rendered) `onException` writeIORef state (TransportBroken accounting))
          `finally` close

closeNativeExchange :: NativeDemandExchange -> IO ()
closeNativeExchange (NativeDemandExchange _ _ _ _ state gate) =
  withMVar gate $ \_ -> do
    accounting <- requestAccounting <$> readIORef state
    writeIORef state (Closed (accounting { sourceStage = Nothing }))


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
        1 -> serve Nothing >> requests channels handler
        6 -> do
          header <- hGetExactly stdin 144
          unless (BS.take 8 header == BSC.pack "TPNDX001") (fail "worker: invalid native exchange header")
          thread <- myThreadId
          state <- newIORef (Ready (NativeAccounting 0 Nothing 0 Nothing))
          gate <- newMVar ()
          let exchange = NativeDemandExchange header thread stdin protocol state gate
          (serve (Just exchange) `finally` closeNativeExchange exchange)
          requests channels handler
        other -> throwIO (userError ("worker: unknown request command " ++ show other))
      where
        serve native = do
          cwd <- decodeText <$> hGetFrame stdin
          argc <- getU32LE <$> hGetExactly stdin 4
          argv <- replicateM (fromIntegral argc) (decodeText <$> hGetFrame stdin)
          let captureBudget = maybe (pure maxResponsePayloadBytes) nativeCaptureBudget native
          (completed, captured) <- captureOutputWithin captureBudget channels
            (handler (phaseSink channels) native cwd argv)
          withNativeTerminal native
            (finishCompletion completed >> fail "worker: native exchange poisoned")
            (\budget -> do
              rendered@(_, out, err) <- renderCapturedWithin budget channels completed captured
              when (toInteger (BS.length out) + toInteger (BS.length err) > budget) $
                fail "worker: terminal response exceeds cumulative native exchange budget"
              pure rendered) $ \(code, out, err) -> do
            emitPhase (phaseSink channels) WritingResponse
            BS.hPut protocol (maybe BS.empty (const (BS.singleton 0)) native
              <> encodeResponse (exitCodeToInt code) out err)
            hFlush protocol
            emitPhase (phaseSink channels) ResponseWritten
            unless (null (completionFailures completed)) $
              finishCompletion completed >> pure ()
            case captured of
              UnavailableCapture -> finishCompletion completed >> fail "worker capture unavailable"
              _ -> pure ()

-- Direct CLI shares capture, release assessment and terminal rendering with the
-- framed owner. Main's callback includes its outer compiler-scope release.
runDirectRequest :: (PhaseSink -> IO (Completion WorkerResponse)) -> IO ExitCode
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

putU64LE :: Word64 -> BS.ByteString
putU64LE value = BS.pack [fromIntegral (value `shiftR` (8 * offset)) | offset <- [0..7]]

getU64LE :: BS.ByteString -> Word64
getU64LE bytes = foldr (.|.) 0
  [fromIntegral (BS.index bytes offset) `shiftL` (8 * offset) | offset <- [0..7]]

frame :: BS.ByteString -> BS.ByteString
frame bytes = putU32LE (fromIntegral (BS.length bytes)) <> bytes

encodeResponse :: Int -> BS.ByteString -> BS.ByteString -> BS.ByteString
encodeResponse code out err =
  putU32LE (fromIntegral code) <> frame out <> frame err

decodeText :: BS.ByteString -> String
decodeText = T.unpack . TE.decodeUtf8With TEE.lenientDecode

captureOutput :: OutputChannels -> IO (Completion WorkerResponse) -> IO (Completion WorkerResponse, CaptureOutcome)
captureOutput = captureOutputWithin (pure maxResponsePayloadBytes)

captureOutputWithin :: IO Integer -> OutputChannels -> IO (Completion WorkerResponse) -> IO (Completion WorkerResponse, CaptureOutcome)
captureOutputWithin remaining channels action = mask $ \restore -> do
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
          budget <- remaining
          readResult <- readCapturedResponse budget outPath errPath
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

renderCaptured :: OutputChannels -> Completion WorkerResponse -> CaptureOutcome -> IO (ExitCode, BS.ByteString, BS.ByteString)
renderCaptured = renderCapturedWithin maxResponsePayloadBytes

renderCapturedWithin :: Integer -> OutputChannels -> Completion WorkerResponse -> CaptureOutcome -> IO (ExitCode, BS.ByteString, BS.ByteString)
renderCapturedWithin budget channels completed captured = do
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
      remaining = budget
        - toInteger (BS.length capturedOut) - toInteger (BS.length capturedErr)
  (code, diagnostics, human) <- renderCompletion (fmap responseDiagnostics completed) assessment
  -- Only the typed successful body supplies a control response. Capture and
  -- release failures retain their diagnostics and can never expose Ready.
  let machine = case (code, completionResult completed) of
        (ExitSuccess, BodySucceeded (ExactInputsResponse outcome)) ->
          encodeEnsureExactInputsOutcomeWithin remaining outcome
        _ -> boundedUtf8 remaining diagnostics
  case machine of
    Just out -> case boundedUtf8 (remaining - toInteger (BS.length out)) human of
      Just debug -> pure (code, out, capturedOut <> capturedErr <> debug)
      Nothing -> budgetFailure
    Nothing -> budgetFailure
  where
    budgetFailure = pure (ExitFailure 1, TE.encodeUtf8 (T.pack (renderDiagsJson ReportWorkerFailure
      [Diag Nothing DiagError ("compiler response diagnostics exceed "
        ++ show maxResponsePayloadBytes ++ " bytes")] ++ "\n")), BS.empty)

responseDiagnostics :: WorkerResponse -> [Diag]
responseDiagnostics (DiagnosticResponse diagnostics) = diagnostics
responseDiagnostics (ExactInputsResponse (ExactInputsRejected cause)) =
  [Diag Nothing DiagError ("exact input acquisition rejected: " ++ show cause)]
responseDiagnostics (ExactInputsResponse _) = []

-- Shared with the Rust response decoder. Prepared/native products remain files;
-- exceeding this diagnostic budget is infrastructure failure, never truncation.
maxResponsePayloadBytes :: Integer
maxResponsePayloadBytes = 16777216

readCapturedResponse :: Integer -> FilePath -> FilePath -> IO (Completion (Either String (BS.ByteString, BS.ByteString)))
readCapturedResponse budget outPath errPath =
  withCompletionRelease CaptureClose (openBinaryFile outPath ReadMode) hClose $ \outHandle ->
  withCompletionRelease CaptureClose (openBinaryFile errPath ReadMode) hClose $ \errHandle ->
  captureCompletion $ do
    outSize <- hFileSize outHandle
    errSize <- hFileSize errHandle
    if outSize + errSize > budget
      then pure (Left ("compiler response capture exceeds " ++ show maxResponsePayloadBytes
        ++ " bytes remaining=" ++ show budget ++ " (stdout=" ++ show outSize ++ ", stderr=" ++ show errSize ++ ")"))
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
