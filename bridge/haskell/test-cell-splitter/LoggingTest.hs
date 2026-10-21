module LoggingTest (runLoggingTests) where

import Control.Exception (bracket, try)
import Control.Monad (unless)
import Data.List (isInfixOf)
import System.Directory (getTemporaryDirectory, removeFile)
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.IO
  ( Handle, IOMode(WriteMode), hClose, openTempFile, readFile', withFile )
import Tidepool.Logging

runLoggingTests :: IO ()
runLoggingTests = do
  checkLevels
  checkRendering
  checkHandles
  checkEnvironment

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

checkRendering :: IO ()
checkRendering = do
  let rendered = renderLogEvent 7 LogTrace "compiler.world" "world.cell.stage"
        [ ("stage", LogText "declaration-inputs-refused")
        , ("count", LogInteger 2)
        , ("ok", LogBool True)
        , ("missing", LogNull)
        , ("stage", LogText "ignored duplicate")
        ]
  unless (all (`isInfixOf` rendered)
      [ "\"schema\":1"
      , "\"level\":\"trace\""
      , "\"target\":\"compiler.world\""
      , "\"event\":\"world.cell.stage\""
      , "\"observed_ns\":7"
      , "\"stage\":\"declaration-inputs-refused\""
      , "\"count\":2"
      , "\"ok\":true"
      , "\"missing\":null"
      ] && not ("ignored duplicate" `isInfixOf` rendered))
    (fail "structured logging encoding changed")
  let escaped = renderLogEvent 9 LogDebug "quote\"target" "line\nbreak"
        [("detail", LogText "quote\" slash\\ newline\n")]
  unless (all (`isInfixOf` escaped)
      ["quote\\\"target", "line\\nbreak", "quote\\\" slash\\\\ newline\\n"])
    (fail "structured logging did not JSON-escape fields")
  let bounded = renderLogEvent 11 LogTrace "target" "bounded"
        [("detail", LogText (replicate 4097 'x'))]
  unless (("\"detail\":\"" ++ replicate 4096 'x' ++ "…\"") `isInfixOf` bounded)
    (fail "structured logging did not bound field values")

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

withClosedHandle :: (Handle -> IO a) -> IO a
withClosedHandle use = bracket acquire release (use . snd)
  where
    acquire = do
      temporary <- getTemporaryDirectory
      (path, handle) <- openTempFile temporary "logging-closed"
      hClose handle
      pure (path, handle)
    release (path, handle) = do
      _ <- try (hClose handle) :: IO (Either IOError ())
      removeFile path

withScratch :: (FilePath -> IO a) -> IO a
withScratch = bracket acquire removeFile
  where
    acquire = do
      temporary <- getTemporaryDirectory
      (path, handle) <- openTempFile temporary "logging-output"
      hClose handle
      pure path

withEnv :: String -> String -> IO a -> IO a
withEnv name value action = bracket (lookupEnv name) restore $ \_ -> setEnv name value >> action
  where
    restore = maybe (unsetEnv name) (setEnv name)
