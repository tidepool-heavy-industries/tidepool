module BoundedReadTest (boundedReadChecks) where

import Control.Exception (IOException, bracket, try)
import Control.Monad (forM_, unless)
import Data.ByteString qualified as BS
import System.Directory (getTemporaryDirectory, removeFile)
import System.IO (hClose, hIsClosed, openBinaryTempFile, IOMode(ReadMode), withBinaryFile)
import Tidepool.BoundedRead (hGetAtMost, readFileAtMost, FileObservation(..), withFileObservations, observeFile)

boundedReadChecks :: IO ()
boundedReadChecks = do
  directory <- getTemporaryDirectory
  bracket (openBinaryTempFile directory "bounded-read")
    (\(path, _) -> removeFile path) $ \(path, output) -> do
      hClose output
      -- Vary content independently of the sentinel ceiling. Larger inputs cross
      -- the reader's chunks, including near the usual 32 KiB boundary.
      forM_ [0, 1, 1024, 32751, 32752, 32753, 32767, 32768, 32769, 131073] $ \size -> do
        let payload = BS.pack (take size (cycle [0..255]))
        BS.writeFile path payload
        forM_ [0, 1, size, size + 1, 32 * 1024 * 1024 + 1] $ \byteCount -> do
          bytes <- readFileAtMost path byteCount
          unless (bytes == BS.take byteCount payload)
            (fail "bounded reader changed bytes or exceeded the requested prefix")
        forM_ [max 0 (size - 1), size, size + 1] $ \bound -> do
          sentinel <- readFileAtMost path (bound + 1)
          unless ((BS.length sentinel > bound) == (size > bound))
            (fail "bounded reader lost the over-limit sentinel")

      let payload = BS.pack (take 131073 (cycle [0..255]))
      BS.writeFile path payload
      (input, bytes) <- withBinaryFile path ReadMode $ \handle -> do
        result <- hGetAtMost handle (32 * 1024 * 1024 + 1)
        pure (handle, result)
      closed <- hIsClosed input
      unless closed (fail "bounded read fixture left its handle open")
      BS.writeFile path BS.empty
      unless (bytes == payload)
        (fail "bounded bytes depended on a closed handle or later file contents")
      failure <- try (hGetAtMost input 1) :: IO (Either IOException BS.ByteString)
      case failure of
        Left _ -> pure ()
        Right _ -> fail "bounded read deferred a closed-handle error beyond its IO action"

      BS.writeFile path payload
      withFileObservations $ \observations -> do
        original <- observeFile observations path (Just (BS.length payload))
        unless (observedByteCount original == BS.length payload)
          (fail "file observation lost its exact byte count")
        -- The same proof can impose a stricter role bound on its observation.
        tooSmall <- try (observeFile observations path (Just (BS.length payload - 1)))
          :: IO (Either IOException FileObservation)
        unless (either (const True) (const False) tooSmall)
          (fail "shared observation bypassed a stricter bound")
        BS.writeFile path (BS.pack [1,2,3])
        repeated <- observeFile observations path Nothing
        unless (repeated == original) (fail "one read-only proof observed its path twice")
        withFileObservations $ \next -> do
          changed <- observeFile next path Nothing
          unless (changed /= original) (fail "file observation survived into the next proof")
      BS.writeFile path payload
      withFileObservations $ \observations -> do
        refused <- try (observeFile observations path (Just 1)) :: IO (Either IOException FileObservation)
        unless (either (const True) (const False) refused) (fail "bounded observation admitted a truncated prefix")
        restored <- observeFile observations path Nothing
        unless (observedByteCount restored == BS.length payload)
          (fail "failed bounded read published a partial observation")
