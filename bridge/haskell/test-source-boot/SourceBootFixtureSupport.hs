{-# LANGUAGE ScopedTypeVariables #-}
module SourceBootFixtureSupport
  ( writeExecutionScope, compactInventoryRows, hasIntResultLiteral, withTiming
  , CapturedCompilerFixture, capturePreparedFixture, captureDiagnostics
  , acquireFixtureScratch, releaseFixtureScratch, releaseFixtureScratchAfterFailure
  , withScratchFailureEvidence
  , requireOriginalSourceRejection, requireOriginalSourceBytesChanged
  , requireSourceSelectionInput, requireUserError
  , requireFailedCompilerTransaction
  , manifest, writeManifestFor, originalCompilerInput, digest, withScratch, preparedNames ) where

import Codec.CBOR.Term (Term(..), encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (SomeException, IOException, bracket, bracketOnError, finally, mask, onException, throwIO, try)
import Control.Monad (filterM, foldM, void)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.ByteString.Char8 qualified as BSC
import Data.List (sort)
import Data.Map.Strict qualified as Map
import GHC.Core qualified as Core
import GHC.Driver.Env (HscEnv(..))
import GHC.Driver.Session (importPaths)
import GHC.Tc.Types (tcg_mod)
import GHC.Types.Literal (Literal(..), LitNumType(..))
import GHC.Types.Name (getOccString)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import GenuineCandidateFixture
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
import Tidepool.GhcPipeline (PreparedPipelineResult(..), PipelineResult(..), CompilerTransactionFailure(..))
import Tidepool.DiagJson (InputRejection(..))
import Tidepool.ExecutionSource (ExecutionSourceFailure(..), ExecutionSourceValidationStage(..))
import Tidepool.PreparedStg (PreparedModule(..))

capturePreparedFixture :: FilePath -> PreparedPipelineResult -> IO CapturedCompilerFixture
capturePreparedFixture work prepared = do
  (source, roots) <- originalCompilerInput prepared
  (capture, diagnostics) <- captureDiagnostics
    (captureCompilerFixture (FixtureCompilerInput work source roots) prepared)
  hPutStr stderr diagnostics
  pure capture

-- Refusal assertions catch only the owning category. Unexpected source,
-- filesystem, process and cancellation failures keep their original exception.
requireOriginalSourceRejection :: String -> ExecutionSourceFailure -> IO a -> IO ()
requireOriginalSourceRejection label expected action = do
  result <- try (void action)
  case result of
    Left (OriginalSourceSelectionRejected actual) | actual == expected -> pure ()
    Left failure -> throwIO failure
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
  result <- try (void action)
  case result of
    Left (OriginalSourceSelectionInputUnavailable actual)
      | actual == show (userError diagnostic) -> pure ()
    Left failure -> throwIO failure
    Right () -> fail (label ++ ": expected source-selection diagnostic " ++ diagnostic)

requireUserError :: String -> String -> IO a -> IO ()
requireUserError label diagnostic action = do
  result <- try (void action)
  case result of
    Left (failure :: IOException)
      | isUserError failure && ioeGetErrorString failure == diagnostic -> pure ()
      | otherwise -> throwIO failure
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


data FixtureInventory = FixtureInventory
  { fixtureSymbols :: Map.Map BS.ByteString Int
  , fixtureSymbolRows :: [Term]
  , fixtureGlobals :: Map.Map BS.ByteString Int
  , fixtureGlobalRows :: [Term]
  }

compactInventoryRows :: [Term] -> Either String (Term,Term,[Term])
compactInventoryRows rows = do
  (inventory,compact) <- mapFixtureInventory compactRow empty rows
  pure (TList (reverse (fixtureSymbolRows inventory)),TList (reverse (fixtureGlobalRows inventory)),compact)
  where
    empty = FixtureInventory Map.empty [] Map.empty []
    compactRow inventory (TList fields) | length fields == 16 = case drop 10 fields of
      TList groups:_ -> do
        (next,compact) <- mapFixtureInventory compactGroup inventory groups
        pure (next,TList (take 10 fields ++ [TList compact] ++ drop 11 fields))
      _ -> Left "fixture candidate lacks groups"
    compactRow _ _ = Left "fixture candidate must have sixteen fields"
    compactGroup inventory (TList [ordinal,TList binders,TList globals]) = do
      (withBinders,binderRefs) <- mapFixtureInventory internFixtureSymbol inventory binders
      (withGlobals,globalRefs) <- mapFixtureInventory internFixtureGlobal withBinders globals
      pure (withGlobals,TList [ordinal,TList binderRefs,TList globalRefs])
    compactGroup _ _ = Left "fixture original group must have three fields"

mapFixtureInventory :: (FixtureInventory -> a -> Either String (FixtureInventory,b))
  -> FixtureInventory -> [a] -> Either String (FixtureInventory,[b])
mapFixtureInventory step initial values = do
  (final,reversed) <- foldM (\(inventory,acc) value -> do
    (next,result) <- step inventory value
    pure (next,result:acc)) (initial,[]) values
  pure (final,reverse reversed)

internFixtureSymbol :: FixtureInventory -> Term -> Either String (FixtureInventory,Term)
internFixtureSymbol inventory value@(TList [_,_,_,_,_]) =
  let key = toStrictByteString (encodeTerm value)
  in case Map.lookup key (fixtureSymbols inventory) of
    Just index -> Right (inventory,TInt index)
    Nothing ->
      let index = Map.size (fixtureSymbols inventory)
      in Right (inventory
        { fixtureSymbols = Map.insert key index (fixtureSymbols inventory)
        , fixtureSymbolRows = value:fixtureSymbolRows inventory },TInt index)
internFixtureSymbol _ _ = Left "fixture symbol must have five fields"

internFixtureGlobal :: FixtureInventory -> Term -> Either String (FixtureInventory,Term)
internFixtureGlobal inventory value@(TList [identity,rep,signature,evaluated,generation]) =
  let key = toStrictByteString (encodeTerm value)
  in case Map.lookup key (fixtureGlobals inventory) of
    Just index -> Right (inventory,TInt index)
    Nothing -> do
      (withSymbol,symbolRef) <- internFixtureSymbol inventory identity
      let index = Map.size (fixtureGlobals withSymbol)
      pure (withSymbol
        { fixtureGlobals = Map.insert key index (fixtureGlobals withSymbol)
        , fixtureGlobalRows = TList [symbolRef,rep,signature,evaluated,generation]:fixtureGlobalRows withSymbol },TInt index)
internFixtureGlobal _ _ = Left "fixture global must have five fields"


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
  source <- case [dependencyModuleSource node | node <- dependencyModules (pprDependencies prepared)
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
