-- | Strict bounded prefixes without reserving the entire byte ceiling.
-- Callers read their limit plus one byte to detect oversized or growing files.
module Tidepool.BoundedRead (readFileAtMost, hGetAtMost) where

import Control.Exception (evaluate)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import System.IO (Handle, IOMode(ReadMode), withBinaryFile)

readFileAtMost :: FilePath -> Int -> IO BS.ByteString
readFileAtMost path count = withBinaryFile path ReadMode $ \handle ->
  hGetAtMost handle count

-- Force every chunk and the strict result before the caller closes its handle.
-- Allocation follows bytes actually read, with bounded chunk overhead.
hGetAtMost :: Handle -> Int -> IO BS.ByteString
hGetAtMost handle count = do
  chunks <- BL.hGet handle (fromIntegral count)
  let bytes = BL.toStrict chunks
  _ <- evaluate (BS.length bytes)
  pure bytes
