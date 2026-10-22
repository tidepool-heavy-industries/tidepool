{-# LANGUAGE OverloadedStrings #-}

module LoggingTest (runLoggingTests) where

import Control.Exception
  ( AsyncException(ThreadKilled), ErrorCall(..), Exception(..), IOException
  , asyncExceptionFromException, asyncExceptionToException, bracket, throw, throwIO
  , try )
import Control.Monad (unless)
import qualified Data.ByteString as BS
import Data.IORef (newIORef, readIORef, writeIORef)
import Data.Int (Int64)
import Data.List (isInfixOf)
import System.Directory (doesPathExist, getTemporaryDirectory, removeFile)
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.IO
  ( Handle, IOMode(WriteMode), hClose, openTempFile, readFile', stderr, withFile )
import Tidepool.ExtractUtil (trySynchronous)
import Tidepool.Logging

-- Cancellation can be wrapped by an application-specific exception type.
data WrappedCancellation = WrappedCancellation deriving (Eq, Show)
instance Exception WrappedCancellation where
  toException = asyncExceptionToException
  fromException = asyncExceptionFromException

runLoggingTests :: IO ()
runLoggingTests = do
  checkLevels
  checkEmission
  checkEscaping
  checkBoundedValue
  checkFieldCap
  checkDuplicateTruncation
  checkWorstCaseBound
  checkHandles
  checkEnvironment
  checkDisabledLaziness
  checkAsyncEscape
  checkPartialAcquireCleanup

checkLevels :: IO ()
checkLevels = do
  let traceLogger = newLogger "tidepool.compiler" LogTrace
      disabled = newLogger "tidepool.compiler" LogOff
      warning = newLogger "tidepool.compiler" LogWarn
  unless (loggerEnabled traceLogger LogTrace
      && loggerEnabled warning LogError
      && not (loggerEnabled warning LogTrace)
      && not (loggerEnabled disabled LogError))
    (fail "logging level boundary changed")
  unless (map logLevelFromName ["off", "error", "warn", "info", "debug", "trace"]
      == [LogOff, LogError, LogWarn, LogInfo, LogDebug, LogTrace]
      && logLevelFromName "TRACE" == LogTrace
      && logLevelFromName "unknown" == LogOff)
    (fail "logging level parsing changed")

-- Observe the real emission path, not a re-implemented renderer: the record
-- lands in a scratch sink and its encoding is checked there.
checkEmission :: IO ()
checkEmission = withScratch $ \path -> do
  let logger = newLogger "compiler.world" LogTrace
  withFile path WriteMode $ \handle ->
    logEventTo handle logger LogTrace "world.cell.stage"
      [ ("stage", LogText "declaration-inputs-refused")
      , ("count", LogInteger 2)
      , ("ok", LogBool True)
      , ("missing", LogNull)
      , ("stage", LogText "ignored duplicate")
      ]
  rendered <- readFile' path
  unless (all (`isInfixOf` rendered)
      [ "\"schema\":1"
      , "\"level\":\"trace\""
      , "\"target\":\"compiler.world\""
      , "\"event\":\"world.cell.stage\""
      , "\"observed_ns\":"
      , "\"stage\":\"declaration-inputs-refused\""
      , "\"count\":2"
      , "\"ok\":true"
      , "\"missing\":null"
      ] && not ("ignored duplicate" `isInfixOf` rendered))
    (fail "structured logging encoding changed")

checkEscaping :: IO ()
checkEscaping = withScratch $ \path -> do
  let logger = newLogger "quote\"target" LogDebug
  withFile path WriteMode $ \handle ->
    logEventTo handle logger LogDebug "line\nbreak"
      [("detail", LogText "quote\" slash\\ newline\n")]
  escaped <- readFile' path
  unless (all (`isInfixOf` escaped)
      ["quote\\\"target", "line\\nbreak", "quote\\\" slash\\\\ newline\\n"])
    (fail "structured logging did not JSON-escape fields")

checkBoundedValue :: IO ()
checkBoundedValue = withScratch $ \path -> do
  let logger = newLogger "target" LogTrace
  withFile path WriteMode $ \handle ->
    logEventTo handle logger LogTrace "bounded"
      [("detail", LogText (replicate 4097 'x'))]
  bounded <- readFile' path
  unless (("\"detail\":\"" ++ replicate 4096 'x' ++ "…\"") `isInfixOf` bounded)
    (fail "structured logging did not bound field values")

checkFieldCap :: IO ()
checkFieldCap = withScratch $ \path -> do
  let logger = newLogger "target" LogTrace
      overflowing =
        [("field" ++ show index, LogInteger index) | index <- [1 .. 32 :: Int64]]
        ++ [error "field cap traversed its omitted tail"]
  withFile path WriteMode $ \handle ->
    logEventTo handle logger LogTrace "field-cap" overflowing
  capped <- readFile' path
  unless ("\"fields_truncated\":true" `isInfixOf` capped)
    (fail "field cap did not encode fixed-width truncation")
  unless ("field32" `isInfixOf` capped)
    (fail "field cap dropped a field inside the retained window")

-- A duplicate name inside the retained window is itself a truncation: the
-- first value wins, the later duplicate is dropped, and the fixed-width
-- marker records the omission without a second unbounded scan.
checkDuplicateTruncation :: IO ()
checkDuplicateTruncation = withScratch $ \path -> do
  let logger = newLogger "target" LogTrace
  withFile path WriteMode $ \handle ->
    logEventTo handle logger LogTrace "duplicate-name"
      [ ("stage", LogText "first")
      , ("stage", LogText "second")
      ]
  rendered <- readFile' path
  unless ("\"fields_truncated\":true" `isInfixOf` rendered)
    (fail "duplicate field name inside the window did not set the truncation marker")
  unless ("first" `isInfixOf` rendered && not ("second" `isInfixOf` rendered))
    (fail "duplicate field name did not keep the first value and drop the later one")

-- Exercise the declared ceiling through the actual UTF-8 sink. Every bounded
-- input is longer than its rendered prefix and uses control characters, the
-- worst JSON escape; the start of an unevaluated overflow tail proves the cap
-- does not scan beyond its fixed field window.
checkWorstCaseBound :: IO ()
checkWorstCaseBound = withScratch $ \path -> do
  let logger = newLogger (replicate 257 '\x01') LogTrace
      event = replicate 257 '\x01'
      fields =
        [ ( show index ++ replicate 128 '\x01' ++ "unique"
          , LogText (replicate 4097 '\x01') )
        | index <- [0 .. 31 :: Int]
        ]
        ++ [error "worst-case bound traversed its omitted tail"]
  withFile path WriteMode $ \handle ->
    logEventTo handle logger LogTrace event fields
  rendered <- BS.readFile path
  unless (BS.length rendered <= maxRenderedLogBytes + 1)
    (fail ("structured logging exceeded its byte ceiling: "
      ++ show (BS.length rendered) ++ " > " ++ show (maxRenderedLogBytes + 1)))
  unless (BS.isInfixOf "\"fields_truncated\":true" rendered)
    (fail "worst-case event omitted its fixed-width truncation marker")

checkHandles :: IO ()
checkHandles = do
  let traceLogger = newLogger "tidepool.compiler" LogTrace
      warning = newLogger "tidepool.compiler" LogWarn
  withClosedHandle $ \handle ->
    logEventTo handle traceLogger LogTrace "closed-handle" [] :: IO ()
  withScratch $ \path -> do
    withFile path WriteMode $ \handle ->
      logEventTo handle warning LogTrace "suppressed" []
    contents <- readFile' path
    unless (null contents) (fail "disabled trace level wrote a diagnostic")
    withFile path WriteMode $ \handle ->
      logEventTo handle warning LogError "visible" [("detail", LogText "present")]
    visible <- readFile' path
    unless ("\"level\":\"error\"" `isInfixOf` visible
        && "\"event\":\"visible\"" `isInfixOf` visible)
      (fail "enabled log level did not write")

checkEnvironment :: IO ()
checkEnvironment = do
  withEnv "TIDEPOOL_LOG" "TrAcE" $ do
    configured <- readLogger "tidepool.compiler"
    unless (loggerEnabled configured LogTrace) (fail "TIDEPOOL_LOG was not case-insensitive")
  withEnv "TIDEPOOL_LOG" "unknown" $ do
    configured <- readLogger "tidepool.compiler"
    unless (not (loggerEnabled configured LogError)) (fail "invalid TIDEPOOL_LOG did not fail closed")

-- A disabled trace must not construct or render its fields: the level check
-- precedes the clock read, rendering and exception boundary.
checkDisabledLaziness :: IO ()
checkDisabledLaziness = do
  let disabled = newLogger "tidepool.compiler" LogOff
      forced = LogText (error "disabled trace forced a field" :: String)
  outcome <- trySynchronous
    (logEventTo stderr disabled LogTrace "lazy-disabled" [("boom", forced)] :: IO ())
  unless (either (const False) (const True) outcome)
    (fail "disabled trace forced an expensive field")

-- A field whose rendering throws a wrapped asynchronous exception must escape
-- the emission boundary: the ordinary-failure catch rethrows SomeAsyncException.
checkAsyncEscape :: IO ()
checkAsyncEscape = withScratch $ \path -> do
  let logger = newLogger "tidepool.compiler" LogTrace
      escaping = LogText (throw WrappedCancellation :: String)
  withFile path WriteMode $ \handle -> do
    escaped <- try (logEventTo handle logger LogTrace "async-escape" [("detail", escaping)] :: IO ())
      :: IO (Either WrappedCancellation ())
    unless (either (== WrappedCancellation) (const False) escaped)
      (fail "wrapped asynchronous exception was swallowed by the emission path")

-- The opened (path, handle) pair is owned before the extra close step runs, so
-- a failure or asynchronous exception during that step still removes the
-- scratch file and re-raises the original exception.
checkPartialAcquireCleanup :: IO ()
checkPartialAcquireCleanup = do
  ordinaryPath <- newIORef Nothing
  let ordinaryClose (path, _) =
        writeIORef ordinaryPath (Just path) >> throwIO (ErrorCall "acquired close failed")
  ordinary <- try (withOwnedTemp "logging-acquire" ordinaryClose (\_ _ -> pure ()))
    :: IO (Either ErrorCall ())
  case ordinary of
    Left _ -> do
      Just path <- readIORef ordinaryPath
      removed <- doesPathExist path
      unless (not removed) (fail "partially acquired scratch file was not removed")
    Right () -> fail "ordinary close failure did not propagate"
  asyncPath <- newIORef Nothing
  let asyncClose (path, _) = writeIORef asyncPath (Just path) >> throwIO ThreadKilled
  async <- try (withOwnedTemp "logging-async-acquire" asyncClose (\_ _ -> pure ()))
    :: IO (Either AsyncException ())
  case async of
    Left ThreadKilled -> do
      Just path <- readIORef asyncPath
      removed <- doesPathExist path
      unless (not removed)
        (fail "partially acquired scratch file was not removed under async cancellation")
    _ -> fail "async exception during acquisition did not escape"

withClosedHandle :: (Handle -> IO a) -> IO a
withClosedHandle use = withOwnedTemp "logging-closed" (hClose . snd) (\_ handle -> use handle)

withScratch :: (FilePath -> IO a) -> IO a
withScratch use = withOwnedTemp "logging-output" (hClose . snd) (\path _ -> use path)

-- Acquire a scratch (path, handle) pair, run the caller's transition (usually
-- closing the handle), then run the consumer. The pair is owned by the release
-- action from the moment it opens, so the extra close step cannot leak the path
-- if it fails or is interrupted.
withOwnedTemp
  :: String
  -> ((FilePath, Handle) -> IO ())
  -> (FilePath -> Handle -> IO a)
  -> IO a
withOwnedTemp template close use = do
  temporary <- getTemporaryDirectory
  bracket (openTempFile temporary template) release $ \(path, handle) -> do
    close (path, handle)
    use path handle
  where
    release (path, handle) = do
      _ <- try (hClose handle) :: IO (Either IOException ())
      removeFile path

withEnv :: String -> String -> IO a -> IO a
withEnv name value action = bracket (lookupEnv name) restore $ \_ -> setEnv name value >> action
  where
    restore = maybe (unsetEnv name) (setEnv name)
