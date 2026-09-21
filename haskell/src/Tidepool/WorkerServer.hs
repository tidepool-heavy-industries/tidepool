-- | Framed stdin/stdout loop for the resident Haskell compiler worker.
-- Process supervision, sockets, timeouts, and rotation belong to the Rust
-- frontend. This module only preserves one GHC session across requests.
module Tidepool.WorkerServer (RequestHandler, runWorkerLoop) where

import Control.Exception (IOException, catch, finally, throwIO)
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
  ( Handle, hClose, hFlush, openTempFile, stderr, stdin, stdout )
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
captureOutput action = do
  tmpDir <- getTemporaryDirectory
  (outPath, outHandle) <- openTempFile tmpDir "tidepool-worker-stdout.txt"
  (errPath, errHandle) <- openTempFile tmpDir "tidepool-worker-stderr.txt"
  let removeTemps = do
        removeFile outPath `catch` \(_ :: IOException) -> pure ()
        removeFile errPath `catch` \(_ :: IOException) -> pure ()
  (do
      exitCode <- runRedirected outHandle errHandle action
      out <- BS.readFile outPath
      err <- BS.readFile errPath
      pure (exitCodeToInt exitCode, out, err)
    ) `finally` removeTemps

runRedirected :: Handle -> Handle -> IO ExitCode -> IO ExitCode
runRedirected outHandle errHandle action = do
  savedOut <- hDuplicate stdout
  savedErr <- hDuplicate stderr
  hDuplicateTo outHandle stdout
  hDuplicateTo errHandle stderr
  action `finally` do
    hFlush stdout
    hFlush stderr
    hDuplicateTo savedOut stdout
    hDuplicateTo savedErr stderr
    hClose savedOut
    hClose savedErr
    hClose outHandle
    hClose errHandle

exitCodeToInt :: ExitCode -> Int
exitCodeToInt ExitSuccess = 0
exitCodeToInt (ExitFailure code) = code
