module Main (main, tests) where

import Control.Exception (bracket)
import System.Directory (createDirectory, getTemporaryDirectory, removeFile, removeDirectoryRecursive)
import System.IO (openTempFile, hClose)
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)
import WorkerResponseCases
  ( check, checkSourceBorrowing, checkSourceBorrowCancellation, checkNativeExchangeFailures, checkPartialAcquisition, checkSourceFreeCleanup, checkCompilerClusterReuse
  , checkCompilerClusterCleanup, checkCompilerClusterCancellation )

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "worker-response"
  [ testCase "response bounds async unwind and next request" (withScratch check)
  , testCase "SOURCE borrow serializes tasks and rejects foreign thread stale request stage and native queries" (withScratch checkSourceBorrowing)
  , testCase "SOURCE borrow cancellation and partial replies poison without deadlock" (withScratch checkSourceBorrowCancellation)
  , testCase "native exchange rejects stale framing and cumulative exhaustion" (withScratch checkNativeExchangeFailures)
  , testCase "source-free cleanup failure suppresses Ready" (withScratch checkSourceFreeCleanup)
  , testCase "partial FD acquisition unwinds and recovers" (withScratch checkPartialAcquisition)
  , testCase "real compiler refusal preserves worker reuse" (withScratch checkCompilerClusterReuse)
  , testCase "real compiler body survives unsafe capture and retires" (withScratch checkCompilerClusterCleanup)
  , testCase "real compiler cancellation retires without terminal report" (withScratch checkCompilerClusterCancellation)
  ]

withScratch :: (FilePath -> IO a) -> IO a
withScratch action = bracket temporary removeDirectoryRecursive action
  where
    temporary = do
      root <- getTemporaryDirectory
      (path, handle) <- openTempFile root "tidepool-response-test"
      hClose handle
      removeFile path
      createDirectory path
      pure path
