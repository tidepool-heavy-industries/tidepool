module HarnessSourceTest (harnessSourceChecks) where

import Control.Concurrent (forkIO, killThread, newEmptyMVar, putMVar, takeMVar, threadDelay)
import Control.Exception (AsyncException(ThreadKilled), IOException, SomeException, bracket, fromException, try)
import Control.Monad (unless)
import qualified Data.ByteString as BS
import GHC.Conc (ThreadStatus(..), threadStatus)
import System.Directory (createDirectory, createDirectoryIfMissing, getTemporaryDirectory, listDirectory, removeDirectoryRecursive, removeFile)
import System.FilePath ((</>))
import System.IO (IOMode(ReadWriteMode), hClose, openTempFile, withFile)
import System.IO.Error (isDoesNotExistError)
import System.Posix.Files (createLink, createNamedPipe, createSymbolicLink, ownerReadMode, ownerWriteMode, unionFileModes)
import System.Timeout (timeout)
import Tidepool.ExtractRequest (RequestField(..), WorkerRequest(..), workerArgv, workerRequestFromArgv)
import Tidepool.HarnessSource (harnessProfilePragmaLine, spliceHarnessProfilePragma)

harnessSourceChecks :: IO ()
harnessSourceChecks = withScratch $ \root -> do
  let first = root </> "first" </> "Input.hs"
      second = root </> "second" </> "Input.hs"
      out = root </> "copies"
      body = "module Input where\nvalue = 1\n"
  createDirectoryIfMissing True (root </> "first")
  createDirectoryIfMissing True (root </> "second")
  writeFile first body
  writeFile second "module Input where\nvalue = 2\n"
  originals <- mapM BS.readFile [first, second]
  ordinary <- request [Input first, Input second, OutputDir out]
  ordinaryCopy <- spliceHarnessProfilePragma ordinary
  equal "ordinary mode only rewrites first input" [out </> "Input.hs", second] (requestFiles ordinaryCopy)
  copied <- readFile (out </> "Input.hs")
  equal "exact pragma and one diagnostic line" (harnessProfilePragmaLine ++ "\n" ++ body) copied
  implicit <- request [Input first]
  implicitCopy <- spliceHarnessProfilePragma implicit
  equal "default output directory" [root </> "first" </> "Input_cbor" </> "Input.hs"] (requestFiles implicitCopy)
  empty <- request [OutputDir out]
  unchanged <- spliceHarnessProfilePragma empty
  equal "empty input remains empty" [] (requestFiles unchanged)
  inspections <- request [Input first, Input second, Input first, OutputDir out,
    InspectInfo "value", InspectInfo "value", InspectInfo "value", InspectTypeBatch first]
  inspectionCopies <- spliceHarnessProfilePragma inspections
  let query0 = out </> "inspection-query-0" </> "Input.hs"
      query1 = out </> "inspection-query-1" </> "Input.hs"
  equal "raw duplicates share indexed copy; basename peers do not" [query0, query1, query0] (requestFiles inspectionCopies)
  directories <- listDirectory out
  unless ("inspection-query-2" `notElem` directories) (fail "duplicate input created another copy")
  equal "batch copy stays separate" (Just (out </> "inspection-type-batch" </> "Input.hs")) (requestInspectTypeBatch inspectionCopies)
  equal "second basename contents" (harnessProfilePragmaLine ++ "\nmodule Input where\nvalue = 2\n") =<< readFile query1
  -- Equivalent spellings deliberately remain separate raw request identities.
  aliases <- request [Input first, Input (root </> "first" </> "." </> "Input.hs"), OutputDir out, InspectInfo "value"]
  aliasCopies <- spliceHarnessProfilePragma aliases
  equal "raw-path identity is preserved" [query0, query1] (requestFiles aliasCopies)
  equal "caller bytes unchanged" originals =<< mapM BS.readFile [first, second]
  missing <- request [Input (root </> "missing"), OutputDir out]
  failure <- try (spliceHarnessProfilePragma missing) :: IO (Either IOException WorkerRequest)
  unless (either isDoesNotExistError (const False) failure) (fail "missing input lost filesystem error")
  directorySource <- request [Input (root </> "first"), OutputDir out]
  expectFileFailure "directory cannot be read as source" (spliceHarnessProfilePragma directorySource)
  blocked <- request [Input first, OutputDir (root </> "blocked")]
  writeFile (root </> "blocked") "not a directory"
  expectFileFailure "output directory creation failure" (spliceHarnessProfilePragma blocked)
  createDirectoryIfMissing True (root </> "write-failure" </> "Input.hs")
  unwritable <- request [Input first, OutputDir (root </> "write-failure")]
  expectFileFailure "destination cannot be written" (spliceHarnessProfilePragma unwritable)
  aliasDestination <- request [Input first, OutputDir (root </> "first" </> ".")]
  expectFileFailure "output aliases caller source" (spliceHarnessProfilePragma aliasDestination)
  createDirectory (root </> "hardlink-output")
  createLink first (root </> "hardlink-output" </> "Input.hs")
  hardlinked <- request [Input first, OutputDir (root </> "hardlink-output")]
  expectFileFailure "hardlink aliases caller source" (spliceHarnessProfilePragma hardlinked)
  createDirectory (root </> "symlink-output")
  createSymbolicLink first (root </> "symlink-output" </> "Input.hs")
  symlinked <- request [Input first, OutputDir (root </> "symlink-output")]
  expectFileFailure "symlink aliases caller source" (spliceHarnessProfilePragma symlinked)
  equal "caller bytes preserved after failures" originals =<< mapM BS.readFile [first, second]
  cancellationCheck root out
  putStrLn "harness source: copying, identity, failure, and cancellation checks passed"

cancellationCheck :: FilePath -> FilePath -> IO ()
cancellationCheck root out = do
  let fifo = root </> "blocked-source.hs"
  createNamedPipe fifo (ownerReadMode `unionFileModes` ownerWriteMode)
  args <- request [Input fifo, OutputDir out]
  withFile fifo ReadWriteMode $ \_ -> do
    result <- newEmptyMVar
    tid <- forkIO $ do
      outcome <- try (spliceHarnessProfilePragma args) :: IO (Either SomeException WorkerRequest)
      putMVar result outcome
    let waitBlocked = do
          state <- threadStatus tid
          case state of
            ThreadBlocked _ -> pure ()
            ThreadFinished -> fail "source copy completed before blocked-read cancellation"
            ThreadDied -> fail "source-copy test thread died"
            ThreadRunning -> threadDelay 1000 >> waitBlocked
    blocked <- timeout 5000000 waitBlocked
    killThread tid
    outcome <- timeout 5000000 (takeMVar result)
    unless (blocked == Just ()) (fail "source read did not block")
    case outcome of
      Just (Left exception) | Just ThreadKilled <- fromException exception -> pure ()
      _ -> fail "harness source swallowed or replaced cancellation"

request :: [RequestField] -> IO WorkerRequest
request fields = case workerRequestFromArgv (workerArgv fields) of
  Right (Just args) -> pure args
  other -> fail ("invalid test request: " ++ show other)

equal :: (Eq a, Show a) => String -> a -> a -> IO ()
equal label expected actual = unless (expected == actual) (fail (label ++ ": " ++ show actual))

expectFileFailure :: String -> IO WorkerRequest -> IO ()
expectFileFailure label action = do
  result <- try action :: IO (Either IOException WorkerRequest)
  unless (either (const True) (const False) result) (fail label)

withScratch :: (FilePath -> IO a) -> IO a
withScratch = bracket acquire removeDirectoryRecursive
  where
    acquire = do
      temporary <- getTemporaryDirectory
      (path, handle) <- openTempFile temporary "tidepool-harness-source-test"
      hClose handle
      removeFile path
      createDirectory path
      pure path
