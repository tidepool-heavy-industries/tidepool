{-# LANGUAGE ScopedTypeVariables #-}
module SourceBootFixtureSupport
  ( writeExecutionScope, hasIntResultLiteral, withTiming
  , CapturedCompilerFixture, capturePreparedFixture, captureDiagnostics
  , acquireFixtureScratch, releaseFixtureScratch, releaseFixtureScratchAfterFailure
  , withScratchFailureEvidence
  , requireOriginalSourceRejection, requireOriginalSourceBytesChanged
  , requireSourceSelectionInput, requireUserError
  , requireFailedCompilerTransaction
  , manifest, writeManifestFor, originalCompilerInput, digest, withScratch, preparedNames ) where

import Control.Exception
  ( Exception(..), SomeAsyncException, SomeException, IOException, bracket, bracketOnError
  , finally, fromException, mask, onException, throwIO, try )
import Control.Monad (filterM, void)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Char8 qualified as BSC
import Data.List (sort)
import GHC.Core qualified as Core
import GHC.Driver.Env (HscEnv(..))
import GHC.Driver.Session (importPaths)
import GHC.Tc.Types (tcg_mod)
import GHC.Types.Literal (Literal(..), LitNumType(..))
import GHC.Types.Name (getOccString)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import Tidepool.Test.GenuineCandidate
  ( CapturedCompilerFixture, FixtureCompilerInput(..), captureCompilerFixture, capturedPreparedNames
  , writeGenuineCandidateManifestFor, writeGenuineExecutionScope )
import Numeric (showHex)
import System.Directory
  ( getTemporaryDirectory, removeFile, createDirectory, removeDirectoryRecursive
  , listDirectory, doesDirectoryExist, doesFileExist, pathIsSymbolicLink, removePathForcibly, canonicalizePath )
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.FilePath ((</>), makeRelative)
import System.IO
  ( openTempFile, hClose, hFlush, hPutStr, hPutStrLn, hSeek, hFileSize, withBinaryFile
  , IOMode(ReadMode), SeekMode(AbsoluteSeek), stderr )
import System.IO.Error (isUserError, ioeGetErrorString)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import Tidepool.DependencyEvidence (DependencyEvidence(..), DependencyModule(..))
import Tidepool.GhcPipeline
  ( PreparedPipelineResult, pprPipelineResult, pprModules, prHscEnv, prTargetTcGblEnv
  , CompilerTransactionFailure(..), preparedFreshDependencies )
import Tidepool.DiagJson (InputRejection(..))
import Tidepool.ExecutionSource (ExecutionSourceFailure(..), ExecutionSourceValidationStage(..))
import Tidepool.PreparedStg (pmModule)

capturePreparedFixture :: FilePath -> PreparedPipelineResult -> IO CapturedCompilerFixture
capturePreparedFixture work prepared = do
  (source, roots) <- originalCompilerInput prepared
  (capture, diagnostics) <- captureDiagnostics
    (captureCompilerFixture (FixtureCompilerInput work source roots) prepared)
  hPutStr stderr diagnostics
  pure capture

-- Refusal assertions catch only the owning category. Unexpected synchronous
-- failures retain their original cause with control context; cancellation escapes.
requireOriginalSourceRejection :: String -> ExecutionSourceFailure -> IO a -> IO ()
requireOriginalSourceRejection label expected action = do
  result <- try (void action) :: IO (Either SomeException ())
  case result of
    Left failure
      | Just (OriginalSourceSelectionRejected actual) <- fromException failure
      , actual == expected -> pure ()
      | Just (_ :: SomeAsyncException) <- fromException failure -> throwIO failure
      | otherwise -> throwIO (RefusalControlFailure
          (label ++ ": expected original source refusal " ++ show expected) failure)
    Right () -> fail (label ++ ": expected original source refusal " ++ show expected)

requireOriginalSourceBytesChanged :: String -> (String,String) -> FilePath -> String -> IO a -> IO ()
requireOriginalSourceBytesChanged label owner path originalSha action = do
  canonical <- canonicalizePath path
  currentSha <- digest <$> BS.readFile canonical
  requireOriginalSourceRejection label
    (ExecutionSourceChangedDuring owner (OriginalSourceBytesChanged canonical originalSha currentSha)) action

-- The source-selection boundary currently wraps a String-returning authority
-- API's userError in InputRejection. Match that exact owner diagnostic.
requireSourceSelectionInput :: String -> String -> IO a -> IO ()
requireSourceSelectionInput label diagnostic action = do
  result <- try (void action) :: IO (Either SomeException ())
  case result of
    Left failure
      | Just (OriginalSourceSelectionInputUnavailable actual) <- fromException failure
      , actual == show (userError diagnostic) -> pure ()
      | Just (_ :: SomeAsyncException) <- fromException failure -> throwIO failure
      | otherwise -> throwIO (RefusalControlFailure
          (label ++ ": expected source-selection diagnostic " ++ diagnostic) failure)
    Right () -> fail (label ++ ": expected source-selection diagnostic " ++ diagnostic)

data RefusalControlFailure = RefusalControlFailure String SomeException
  deriving (Show)

instance Exception RefusalControlFailure where
  displayException (RefusalControlFailure context cause) =
    context ++ "; original exception: " ++ displayException cause

requireUserError :: String -> String -> IO a -> IO ()
requireUserError label diagnostic action = do
  result <- try (void action) :: IO (Either SomeException ())
  case result of
    Left failure
      | Just (ioFailure :: IOException) <- fromException failure
      , isUserError ioFailure && ioeGetErrorString ioFailure == diagnostic -> pure ()
      | Just (_ :: SomeAsyncException) <- fromException failure -> throwIO failure
      | otherwise -> throwIO (RefusalControlFailure
          (label ++ ": expected owner diagnostic " ++ diagnostic) failure)
    Right () -> fail (label ++ ": expected owner diagnostic " ++ diagnostic)

requireFailedCompilerTransaction :: String -> IO a -> IO ()
requireFailedCompilerTransaction label action = do
  result <- try (void action)
  case result of
    Left CompilerTransactionFailed -> pure ()
    Left failure -> throwIO failure
    Right () -> fail (label ++ ": cancelled compiler transaction remained usable")

writeExecutionScope :: FilePath -> CapturedCompilerFixture -> [String] -> IO FilePath
writeExecutionScope work capture lexicalNames =
  writeGenuineExecutionScope (capturedPreparedNames capture) lexicalNames work capture

hasIntResultLiteral :: Integer -> [Core.CoreBind] -> Bool
hasIntResultLiteral expected = any (\case
      Core.NonRec binder rhs -> getOccString binder == "__result" && contains rhs
      Core.Rec bindings -> any (\(binder, rhs) -> getOccString binder == "__result" && contains rhs) bindings)
  where
    contains = \case
      Core.Lit (LitNumber LitNumInt value) -> value == expected
      Core.App function argument -> contains function || contains argument
      Core.Lam _ body -> contains body
      Core.Let binding body -> any (contains . snd) (Core.flattenBinds [binding]) || contains body
      Core.Case scrutinee _ _ alternatives -> contains scrutinee
        || any (\(Core.Alt _ _ rhs) -> contains rhs) alternatives
      Core.Cast body _ -> contains body
      Core.Tick _ body -> contains body
      _ -> False


withTiming :: IO a -> IO a
withTiming action = bracket (lookupEnv "TIDEPOOL_TIMING") restore $ \_ ->
  setEnv "TIDEPOOL_TIMING" "1" >> action
  where restore = maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING")


manifest :: FilePath -> FilePath
manifest work = work </> "module-candidates.cbor"


writeManifestFor :: [String] -> FilePath -> CapturedCompilerFixture -> IO ()
writeManifestFor = writeGenuineCandidateManifestFor

originalCompilerInput :: PreparedPipelineResult -> IO (FilePath, [FilePath])
originalCompilerInput prepared = do
  let result = pprPipelineResult prepared
      target = tcg_mod (prTargetTcGblEnv result)
      name = moduleNameString (moduleName target)
      unit = unitString (moduleUnit target)
  source <- case [dependencyModuleSource node | node <- dependencyModules (preparedFreshDependencies prepared)
      , dependencyModuleUnit node == unit, dependencyModuleName node == name
      , not (dependencyModuleBoot node)] of
    [path] -> pure path
    _ -> fail "fixture compiler input has no unique captured target source"
  pure (source, importPaths (hsc_dflags (prHscEnv result)))


digest :: BS.ByteString -> String
digest = concatMap (\byte -> let text = showHex byte ""
  in replicate (2 - length text) '0' ++ text) . BS.unpack . SHA.hash


-- Failed cases emit bounded evidence to the existing test runner's retained
-- output. No case owns a second evidence directory or snapshots ambient caches.
acquireFixtureScratch :: IO FilePath
acquireFixtureScratch = do
  root <- getTemporaryDirectory
  bracketOnError (openTempFile root "tidepool-source-boot-test")
    (\(path, handle) -> bestEffort (hClose handle `finally` removePathForcibly path)) $ \(path, handle) -> do
      hClose handle
      removeFile path
      createDirectory path
      pure path

releaseFixtureScratch :: FilePath -> IO ()
releaseFixtureScratch = removeDirectoryRecursive

releaseFixtureScratchAfterFailure :: FilePath -> IO ()
releaseFixtureScratchAfterFailure path = do
  bestEffort (retainScratchFailure path)
  bestEffort (releaseFixtureScratch path)

withScratch :: (FilePath -> IO a) -> IO a
withScratch action = mask $ \restore -> do
  path <- acquireFixtureScratch
  outcome <- try (restore (action path))
  case outcome of
    Right result -> releaseFixtureScratch path >> pure result
    Left (failure :: SomeException) -> do
      releaseFixtureScratchAfterFailure path
      throwIO failure

withScratchFailureEvidence :: FilePath -> IO a -> IO a
withScratchFailureEvidence path action = do
  outcome <- try action
  case outcome of
    Right result -> pure result
    Left (failure :: SomeException) -> do
      bestEffort (retainScratchFailure path)
      throwIO failure

bestEffort :: IO () -> IO ()
bestEffort action = do
  _ <- try action :: IO (Either SomeException ())
  pure ()

retainScratchFailure :: FilePath -> IO ()
retainScratchFailure root = do
  hPutStrLn stderr "source-boot failed scratch: bounded original inputs (hex; 32 files, 8192 bytes/file, 128 entries)"
  walk 128 32 [root]
  where
    walk :: Int -> Int -> [FilePath] -> IO ()
    walk _ _ [] = pure ()
    walk entries files _ | entries <= 0 || files <= 0 =
      hPutStrLn stderr "source-boot failure inputs truncated at evidence bound"
    walk entries files (path:pending) = do
      symbolic <- pathIsSymbolicLink path
      directory <- doesDirectoryExist path
      file <- doesFileExist path
      if symbolic then walk (entries-1) files pending
      else if directory then do
        children <- map (path </>) . take 128 . sort <$> listDirectory path
        directories <- filterM doesDirectoryExist children
        otherInputs <- filterM doesFileExist children
        -- Capture packets precede loose compiler outputs within the bound.
        walk (entries-1) files (take 32 (directories ++ otherInputs) ++ pending)
      else if file then do
        (size, bytes) <- withBinaryFile path ReadMode $ \handle ->
          (,) <$> hFileSize handle <*> BS.hGet handle 8192
        hPutStrLn stderr ("source-boot input " ++ show (makeRelative root path)
          ++ " bytes=" ++ show size ++ " retained=" ++ show (BS.length bytes)
          ++ " retained-sha256=" ++ digest bytes ++ " hex=" ++ bytesHex bytes)
        walk (entries-1) (files-1) pending
      else walk (entries-1) files pending
    bytesHex :: BS.ByteString -> String
    bytesHex = concatMap (\byte -> let value = showHex byte ""
      in replicate (2-length value) '0' ++ value) . BS.unpack

-- Compiler diagnostics are scoped process state. Failure replay is bounded,
-- restores stderr first, and cannot replace the original compiler exception.
captureDiagnostics :: IO a -> IO (a, String)
captureDiagnostics action = mask $ \restore -> do
  temporary <- getTemporaryDirectory
  (path, output) <- openTempFile temporary "source-boot-diagnostics.log"
  let cleanup = hClose output `finally` removeFile path
      readDiagnostics = do
        hSeek output AbsoluteSeek 0
        size <- hFileSize output
        bytes <- BS.hGet output (4 * 1024 * 1024)
        pure (BSC.unpack bytes ++ if size > 4 * 1024 * 1024
          then "\n[compiler diagnostics truncated at 4 MiB]\n" else "")
  outcome <- try $ do
    hFlush stderr
    bracket (hDuplicate stderr) (bestEffort . hClose) $ \saved -> do
      result <- try (restore (hDuplicateTo output stderr >> action))
      let restoreStderr = hFlush stderr `finally` hDuplicateTo saved stderr
      case result of
        Right value -> restoreStderr >> pure value
        Left (failure :: SomeException) -> bestEffort restoreStderr >> throwIO failure
  case outcome of
    Right result -> do
      diagnostics <- readDiagnostics `onException` bestEffort cleanup
      cleanup
      pure (result, diagnostics)
    Left (failure :: SomeException) -> do
      bestEffort $ readDiagnostics >>= hPutStrLn stderr .
        ("source-boot compiler diagnostics (bounded):\n" ++)
      bestEffort cleanup
      throwIO failure

preparedNames :: PreparedPipelineResult -> [String]
preparedNames = map (moduleNameString . moduleName . pmModule) . pprModules
