{-# LANGUAGE ScopedTypeVariables #-}
module SourceBootFixtureSupport
  ( rootedProfilePath, rootedProfileSha256, rootedProfileIdentity, withRootedCompilerEnvironment, withQuietStdout
  , writeExecutionScope, hasIntResultLiteral, withTiming
  , CapturedCompilerFixture, capturePreparedFixture, captureDiagnostics
  , CapturedDiagnosticCounters, capturedDiagnosticCounterValues, DiagnosticCounterCaptureFailure(..)
  , captureDiagnosticsWithCounters
  , acquireFixtureScratch, releaseFixtureScratch, releaseFixtureScratchAfterFailure
  , withScratchFailureEvidence
  , requireOriginalSourceRejection, requireOriginalSourceBytesChanged
  , requireSourceSelectionInput, requireUserError
  , requireFailedCompilerTransaction
  , manifest, writeManifestFor, originalCompilerInput, digest, withScratch, preparedNames ) where

import Control.Exception
  ( Exception(..), SomeAsyncException, SomeException, IOException, bracket, bracketOnError
  , finally, fromException, mask, onException, throwIO, try, evaluate )
import Control.Monad (filterM, foldM, unless, void)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Char8 qualified as BS8
import Data.Char (isSpace)
import Data.Map.Strict qualified as Map
import Data.Text qualified as Text
import Data.Text.Encoding qualified as TextEncoding
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
  , Handle, IOMode(ReadMode), SeekMode(AbsoluteSeek), stderr, stdout )
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
captureDiagnostics action = do
  (result,_,diagnostics) <- captureDiagnosticsWithCounters [] action
  pure (result,diagnostics)

newtype CapturedDiagnosticCounters = CapturedDiagnosticCounters (Map.Map String [Integer])

capturedDiagnosticCounterValues :: CapturedDiagnosticCounters -> String -> Maybe [Integer]
capturedDiagnosticCounterValues (CapturedDiagnosticCounters counters) name = Map.lookup name counters

data DiagnosticCounterCaptureFailure
  = DiagnosticCounterSelectionInvalid String
  | DiagnosticCounterMalformed String
  | DiagnosticCounterOccurrenceOverflow String Int Int
  deriving (Eq, Show)
instance Exception DiagnosticCounterCaptureFailure

data DiagnosticCounterSlot = DiagnosticCounterSlot String Int Int [Integer]

-- Counter rows are small protocol records, independent of arbitrarily long
-- diagnostic/source lines. This fixture budget bounds only selected record
-- parsing; unrelated lines are streamed and discarded without retaining them.
diagnosticCounterLineBudget :: Int
diagnosticCounterLineBudget = 4096

selectDiagnosticCounters :: [(String,Int)] -> IO (Map.Map BS.ByteString DiagnosticCounterSlot)
selectDiagnosticCounters requested = do
  let selected = Map.fromList
        [(TextEncoding.encodeUtf8 (Text.pack name), DiagnosticCounterSlot name bound 0 [])
        | (name,bound) <- requested]
  unless (Map.size selected == length requested)
    (throwIO (DiagnosticCounterSelectionInvalid "duplicate requested counter"))
  mapM_ (\(name,bound) -> unless (not (null name) && not (any isSpace name) && bound >= 0
      && BS.length (TextEncoding.encodeUtf8 (Text.pack name)) <= diagnosticCounterLineBudget - 128)
    (throwIO (DiagnosticCounterSelectionInvalid name))) requested
  pure selected

readDiagnosticCounters :: Map.Map BS.ByteString DiagnosticCounterSlot -> Handle
  -> IO CapturedDiagnosticCounters
readDiagnosticCounters selected output
  | Map.null selected = pure (CapturedDiagnosticCounters Map.empty)
  | otherwise = do
      hSeek output AbsoluteSeek 0
      complete <- go selected (Just BS.empty)
      pure (CapturedDiagnosticCounters (Map.fromList
        [(name,reverse values) | DiagnosticCounterSlot name _ _ values <- Map.elems complete]))
  where
    requested line = case BS8.words line of
      marker:field:_ | marker == BS8.pack "tidepool-count"
        , Just name <- BS8.stripPrefix (BS8.pack "name=") field -> Map.lookup name selected
      _ -> Nothing
    admit known line = case requested line of
      Nothing -> pure known
      Just (DiagnosticCounterSlot name _ _ _) -> do
        value <- case [raw | field <- BS8.words line
            , Just raw <- [BS8.stripPrefix (BS8.pack "count=") field]] of
          [raw] -> case reads (BS8.unpack raw) of
            [(value,"")] -> pure value
            _ -> throwIO (DiagnosticCounterMalformed name)
          _ -> throwIO (DiagnosticCounterMalformed name)
        let key = TextEncoding.encodeUtf8 (Text.pack name)
        case Map.lookup key known of
          Just (DiagnosticCounterSlot _ bound count values)
            | count >= bound -> throwIO (DiagnosticCounterOccurrenceOverflow name (count+1) bound)
            | otherwise -> pure (Map.insert key (DiagnosticCounterSlot name bound (count+1) (value:values)) known)
          Nothing -> throwIO (DiagnosticCounterSelectionInvalid name)
    consume complete (known,carry) piece = case carry of
      Nothing -> pure (known,if complete then Just BS.empty else Nothing)
      Just previous -> do
        let line = previous <> piece
        if BS.length line > diagnosticCounterLineBudget then do
          case requested line of
            Just (DiagnosticCounterSlot name _ _ _) -> throwIO (DiagnosticCounterMalformed name)
            Nothing -> pure ()
          pure (known,if complete then Just BS.empty else Nothing)
        else if complete then do
          next <- admit known line
          pure (next,Just BS.empty)
        else pure (known,Just line)
    go known carry = do
      chunk <- BS.hGetSome output (64 * 1024)
      if BS.null chunk then case carry of
        Just line -> admit known line
        Nothing -> pure known
      else do
        let pieces = BS.split 10 chunk
        state <- foldM (consume True) (known,carry) (init pieces)
        (next,pending) <- consume False state (last pieces)
        go next pending

-- Requested counters are read from the complete temporary file before its
-- independent display prefix is truncated. Occurrence bounds are explicit
-- fixture expectations; overflow and malformed selected rows refuse evidence.
captureDiagnosticsWithCounters :: [(String,Int)] -> IO a -> IO (a, CapturedDiagnosticCounters, String)
captureDiagnosticsWithCounters requested action = mask $ \restore -> do
  selected <- selectDiagnosticCounters requested
  temporary <- getTemporaryDirectory
  (path, output) <- openTempFile temporary "source-boot-diagnostics.log"
  let cleanup = hClose output `finally` removeFile path
      readDiagnostics = do
        hSeek output AbsoluteSeek 0
        size <- hFileSize output
        bytes <- BS.hGet output (4 * 1024 * 1024)
        text <- if size > 4 * 1024 * 1024
          then case TextEncoding.streamDecodeUtf8 bytes of
            TextEncoding.Some prefix _ _ -> do
              -- Only the streaming decoder's incomplete trailing sequence is
              -- omitted. Malformed bytes inside this bounded prefix still fail.
              _ <- evaluate (Text.length prefix)
              pure (Text.unpack prefix)
          else either throwIO (pure . Text.unpack) (TextEncoding.decodeUtf8' bytes)
        pure (text ++ if size > 4 * 1024 * 1024
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
      counters <- readDiagnosticCounters selected output `onException` bestEffort cleanup
      diagnostics <- readDiagnostics `onException` bestEffort cleanup
      cleanup
      pure (result,counters,diagnostics)
    Left (failure :: SomeException) -> do
      bestEffort $ readDiagnostics >>= hPutStrLn stderr .
        ("source-boot compiler diagnostics (bounded):\n" ++)
      bestEffort cleanup
      throwIO failure

preparedNames :: PreparedPipelineResult -> [String]
preparedNames = map (moduleNameString . moduleName . pmModule) . pprModules


rootedProfilePath :: String
rootedProfilePath = "/nix/store/bfa5qsshnhgxihzxf2klvzxx0d2pass9-compiler-package-profile-v1"

rootedProfileSha256 :: String
rootedProfileSha256 = "6722c5a9ea70c599d76a874dc6c4c014d1264abd700537293f9f347ab6d745ce"

rootedProfileIdentity :: String
rootedProfileIdentity = "15663ac19c31058a9203f6404f72cad491923a1f34209bc68cff3bbbf3111880"

withRootedCompilerEnvironment :: FilePath -> IO a -> IO a
withRootedCompilerEnvironment worldDirectory action = bracket
  (traverse (\name -> (name,) <$> lookupEnv name) names)
  (mapM_ (\(name,value) -> maybe (unsetEnv name) (setEnv name) value)) $ \_ -> do
    unsetEnv "LD_LIBRARY_PATH"
    unsetEnv "LD_PRELOAD"
    unsetEnv "LD_AUDIT"
    setEnv "TIDEPOOL_COMPILER_PACKAGE_PROFILE" rootedProfilePath
    setEnv "TIDEPOOL_COMPILER_PACKAGE_PROFILE_SHA256" rootedProfileSha256
    setEnv "TIDEPOOL_COMPILER_SERVICE_EPOCH" (replicate 64 '0')
    setEnv "TIDEPOOL_COMPILER_PACKAGE_SOURCE_ID" (replicate 64 '0')
    setEnv "TIDEPOOL_COMPILER_PACKAGE_PROFILE_ID" rootedProfileIdentity
    setEnv "TIDEPOOL_COMPILER_WORKER_INSTANCE" (replicate 64 '0')
    setEnv "TIDEPOOL_COMPILER_WORLD_DIRECTORY" worldDirectory
    action
  where
    names =
      [ "TIDEPOOL_COMPILER_PACKAGE_PROFILE"
      , "TIDEPOOL_COMPILER_PACKAGE_PROFILE_SHA256"
      , "TIDEPOOL_COMPILER_SERVICE_EPOCH"
      , "TIDEPOOL_COMPILER_PACKAGE_SOURCE_ID"
      , "TIDEPOOL_COMPILER_PACKAGE_PROFILE_ID"
      , "TIDEPOOL_COMPILER_WORKER_INSTANCE"
      , "TIDEPOOL_COMPILER_WORLD_DIRECTORY"
      , "LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT"
      ]

-- Rooted boot acknowledges its association on stdout. Keep that binary frame
-- out of the test runner's protocol while the real association is exercised.
withQuietStdout :: IO a -> IO a
withQuietStdout action = mask $ \restore -> do
  hFlush stdout
  temporary <- getTemporaryDirectory
  (path,output) <- openTempFile temporary "rooted-boot-ack"
  let cleanup = hClose output `finally` removeFile path
  bracket (hDuplicate stdout) hClose $ \saved -> do
    (do hDuplicateTo output stdout
        restore action `finally` (hFlush stdout >> hDuplicateTo saved stdout))
      `finally` cleanup
