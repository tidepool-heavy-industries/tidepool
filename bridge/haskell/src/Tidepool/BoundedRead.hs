-- | Strict bounded prefixes without reserving the entire byte ceiling.
-- Callers read their limit plus one byte to detect oversized or growing files.
module Tidepool.BoundedRead
  ( readFileAtMost, hGetAtMost, fileObservationTotals
  , FileObservations, FileObservation(..), withFileObservations, observeFile ) where

import Control.Exception (evaluate)
import Control.Monad (when)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.IORef (IORef, newIORef, readIORef, modifyIORef')
import Data.Map.Strict qualified as Map
import Numeric (showHex)
import System.IO (Handle, IOMode(ReadMode), withBinaryFile)
import Tidepool.Timing (readTimingEnabled, emitCount)

-- One read-only proof observes each literal path once. Only its digest and
-- length survive the read; this owner must not span compiler effects or proofs.
newtype FileObservations = FileObservations (IORef (Map.Map FilePath FileObservation))

data FileObservation = FileObservation
  { observedSha256 :: String, observedByteCount :: Int } deriving (Eq, Show)

withFileObservations :: (FileObservations -> IO a) -> IO a
withFileObservations action = newIORef Map.empty >>= action . FileObservations

fileObservationTotals :: FileObservations -> IO (Int,Integer)
fileObservationTotals (FileObservations reference) = do
  observations <- readIORef reference
  pure (Map.size observations, sum (map (toInteger . observedByteCount) (Map.elems observations)))

observeFile :: FileObservations -> FilePath -> Maybe Int -> IO FileObservation
observeFile (FileObservations reference) path bound = do
  previous <- Map.lookup path <$> readIORef reference
  observation <- case previous of
    Just value -> pure value
    Nothing -> do
      bytes <- maybe (BS.readFile path) (readFileAtMost path . (+ 1)) bound
      let count = BS.length bytes
          sha = concatMap byteHex (BS.unpack (SHA.hash bytes))
      when (maybe False (count >) bound)
        (fail "exact scope artifact exceeds its byte bound")
      _ <- evaluate (length sha)
      let value = FileObservation sha count
      modifyIORef' reference (Map.insert path value)
      timing <- readTimingEnabled
      emitCount timing ("hash_bytes.observed_file." ++ sha) (fromIntegral count)
      pure value
  when (maybe False (observedByteCount observation >) bound)
    (fail "exact scope artifact exceeds its byte bound")
  pure observation
  where byteHex byte = let digits = showHex byte ""
                      in replicate (2 - length digits) '0' ++ digits

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
