module Main (main) where

import Control.Exception (bracket)
import Control.Monad (unless, void)
import Data.Bits ((.|.), shiftL, shiftR)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BSC
import Data.Word (Word32)
import System.Directory (createDirectory, getTemporaryDirectory, listDirectory, removeFile, removeDirectoryRecursive)
import System.Environment (getArgs, getExecutablePath, setEnv)
import System.Exit (ExitCode(..))
import System.IO (Handle, hClose, hFlush, openTempFile, stdout, stderr)
import System.Process (CreateProcess(..), StdStream(..), createProcess, proc, terminateProcess, waitForProcess)
import System.Timeout (timeout)
import Tidepool.WorkerServer (runWorkerLoop)

limit :: Int
limit = 16777216

main :: IO ()
main = getArgs >>= \case
  ["--worker", scratch] -> do
    setEnv "TMPDIR" scratch
    runWorkerLoop (\transaction -> transaction handler)
  [] -> bracket temporary removeDirectoryRecursive check
  _ -> fail "unexpected worker response test arguments"
  where
    handler _ args = do
      case args of
        ["stdout-overflow"] -> BS.hPut stdout (BS.replicate (limit + 1) 111)
        ["exact-aggregate"] -> do
          BS.hPut stdout (BS.replicate (limit `div` 2) 111)
          BS.hPut stderr (BS.replicate (limit `div` 2) 101)
        ["aggregate-overflow"] -> do
          BS.hPut stdout (BS.replicate (limit `div` 2) 111)
          BS.hPut stderr (BS.replicate (limit `div` 2 + 1) 101)
        ["next-request"] -> BSC.hPutStr stdout "complete-next-request"
        _ -> fail "unexpected capture fixture request"
      pure ExitSuccess

    temporary = do
      root <- getTemporaryDirectory
      (path, handle) <- openTempFile root "tidepool-response-test"
      hClose handle
      -- The tempfile name gives this test its exclusively owned directory.
      removeFile path
      createDirectory path
      pure path

check :: FilePath -> IO ()
check scratch = do
  executable <- getExecutablePath
  bracket (createProcess (proc executable ["--worker", scratch])
      { std_in = CreatePipe, std_out = CreatePipe, std_err = NoStream })
    (\(_, _, _, child) -> terminateProcess child >> void (waitForProcess child)) $
    \(inputHandle, outputHandle, _, child) -> do
      input <- maybe (fail "missing worker stdin pipe") pure inputHandle
      output <- maybe (fail "missing worker stdout pipe") pure outputHandle
      BS.hPut input (BS.singleton 1)
      hFlush input
      ack <- exact output 1
      unless (ack == BS.singleton 1) (fail "missing transaction acknowledgement")
      forRequest input output "stdout-overflow" $ \code out err -> do
        unless (code == 1 && BSC.isInfixOf "worker-failure" out && BS.null err)
          (fail "oversized stdout did not produce bounded infrastructure failure")
        unless (BS.length out < 1024) (fail "overflow diagnostic is not bounded")
      forRequest input output "exact-aggregate" $ \code out err ->
        unless (code == 0 && out == BS.replicate (limit `div` 2) 111
            && err == BS.replicate (limit `div` 2) 101)
          (fail "exact aggregate boundary was truncated or refused")
      forRequest input output "aggregate-overflow" $ \code out err ->
        unless (code == 1 && BSC.isInfixOf "capture exceeds" out && BS.null err)
          (fail "aggregate capture overflow was not refused")
      forRequest input output "next-request" $ \code out err ->
        unless (code == 0 && out == "complete-next-request" && BS.null err)
          (fail "worker did not continue after oversized capture")
      BS.hPut input (BS.singleton 0)
      hFlush input
      ackDone <- exact output 1
      unless (ackDone == BS.singleton 1) (fail "missing completed transaction acknowledgement")
      hClose input
      status <- waitForProcess child
      unless (status == ExitSuccess) (fail "worker failed to close cleanly")
      leftovers <- listDirectory scratch
      unless (null leftovers) (fail ("capture tempfiles retained: " ++ show leftovers))
      putStrLn "worker response bounds: 1 passed"

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
