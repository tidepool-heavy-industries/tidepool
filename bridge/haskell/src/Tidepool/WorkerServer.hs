{-# LANGUAGE ScopedTypeVariables #-}

-- | Framed stdin/stdout loop for the resident Haskell compiler worker.
-- Process supervision, sockets, timeouts, and rotation belong to the Rust
-- frontend. This module only preserves one GHC session across requests.
module Tidepool.WorkerServer (RequestHandler, runWorkerLoop) where

import Control.Exception
  ( SomeException, mask, throwIO, try )
import Control.Monad (replicateM)
import Data.Bits ((.|.), shiftL, shiftR)
import qualified Data.ByteString as BS
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.Text.Encoding.Error as TEE
import Data.Word (Word32)
import System.Directory (getTemporaryDirectory, removeFile)
import System.Exit (ExitCode(..))
import System.IO
  ( Handle, IOMode(ReadMode), hClose, hFileSize, hFlush, openTempFile
  , stderr, stdin, stdout, withBinaryFile )
import Tidepool.DiagJson (Diag(..), DiagSeverity(..), ReportOutcome(..), renderDiagsJson)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)

type RequestHandler = FilePath -> [String] -> IO ExitCode

-- | Serve explicitly bracketed compiler transactions. The callback owns the
-- compiler-state bracket and invokes its argument while that state may be
-- reused. A truncated transaction throws through the callback, so its
-- cleanup runs before the worker exits.
runWorkerLoop :: ((RequestHandler -> IO ()) -> IO ()) -> IO ()
runWorkerLoop withTransaction = go
  where
    go = do
      command <- BS.hGet stdin 1
      if BS.null command
        then pure ()
        else case BS.head command of
          1 -> do
            withTransaction serveTransaction
            BS.hPut stdout (BS.singleton 1)
            hFlush stdout
            go
          other -> throwIO (userError ("worker: unknown transaction command " ++ show other))

    serveTransaction handler = do
      BS.hPut stdout (BS.singleton 1)
      hFlush stdout
      requests handler

    requests handler = do
      command <- hGetExactly stdin 1
      case BS.head command of
        0 -> pure ()
        1 -> do
          cwd <- decodeText <$> hGetFrame stdin
          argc <- getU32LE <$> hGetExactly stdin 4
          argv <- replicateM (fromIntegral argc) (decodeText <$> hGetFrame stdin)
          (code, out, err) <- captureOutput (handler cwd argv)
          BS.hPut stdout (encodeResponse code out err)
          hFlush stdout
          requests handler
        other -> throwIO (userError ("worker: unknown request command " ++ show other))

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

captureOutput :: IO ExitCode -> IO (Int, BS.ByteString, BS.ByteString)
captureOutput action = mask $ \restore -> do
  tmpDir <- getTemporaryDirectory
  (outPath, outHandle) <- openTempFile tmpDir "tidepool-worker-stdout.txt"
  errTemp <- try (openTempFile tmpDir "tidepool-worker-stderr.txt")
  (errPath, errHandle) <- case errTemp of
    Right pair -> pure pair
    Left (failure :: SomeException) -> do
      ignoreFailures [hClose outHandle, removeFile outPath]
      throwIO failure
  result <- try $ restore $ do
    exitCode <- runRedirected outHandle errHandle action
    closeFailures <- attemptAll [hClose outHandle, hClose errHandle]
    throwFirst closeFailures
    captured <- readCapturedResponse outPath errPath
    pure $ case captured of
      Right (out, err) -> (exitCodeToInt exitCode, out, err)
      Left message -> (1, TE.encodeUtf8 (T.pack
        (renderDiagsJson ReportWorkerFailure [Diag Nothing DiagError message])), BS.empty)
  ignoreFailures [hClose outHandle, hClose errHandle]
  cleanupFailures <- attemptAll [removeFile outPath, removeFile errPath]
  case result of
    Left (failure :: SomeException) -> throwIO failure
    Right value -> throwFirst cleanupFailures >> pure value

-- Attempt every release even if an earlier one fails. The operation's
-- exception takes precedence; otherwise the first cleanup failure is visible.
attemptAll :: [IO ()] -> IO [SomeException]
attemptAll = go []
  where
    go failures [] = pure (reverse failures)
    go failures (operation : rest) = do
      result <- try operation
      case result of
        Left (failure :: SomeException) -> go (failure : failures) rest
        Right () -> go failures rest

ignoreFailures :: [IO ()] -> IO ()
ignoreFailures operations = attemptAll operations >> pure ()

throwFirst :: [SomeException] -> IO ()
throwFirst [] = pure ()
throwFirst (failure : _) = throwIO failure

-- Shared with the Rust response decoder. Prepared/native products remain files;
-- exceeding this diagnostic budget is infrastructure failure, never truncation.
maxResponsePayloadBytes :: Integer
maxResponsePayloadBytes = 16777216

readCapturedResponse :: FilePath -> FilePath -> IO (Either String (BS.ByteString, BS.ByteString))
readCapturedResponse outPath errPath =
  withBinaryFile outPath ReadMode $ \outHandle ->
  withBinaryFile errPath ReadMode $ \errHandle -> do
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

runRedirected :: Handle -> Handle -> IO ExitCode -> IO ExitCode
runRedirected outHandle errHandle action = mask $ \restore -> do
  savedOut <- hDuplicate stdout
  savedErrResult <- try (hDuplicate stderr)
  savedErr <- case savedErrResult of
    Right handle -> pure handle
    Left (failure :: SomeException) -> do
      ignoreFailures [hClose savedOut]
      throwIO failure
  result <- try $ restore $ do
    hDuplicateTo outHandle stdout
    hDuplicateTo errHandle stderr
    action
  cleanupFailures <- attemptAll
    [ hFlush stdout, hFlush stderr
    , hDuplicateTo savedOut stdout, hDuplicateTo savedErr stderr
    , hClose savedOut, hClose savedErr
    ]
  case result of
    Left (failure :: SomeException) -> throwIO failure
    Right value -> throwFirst cleanupFailures >> pure value

exitCodeToInt :: ExitCode -> Int
exitCodeToInt ExitSuccess = 0
exitCodeToInt (ExitFailure code) = code
