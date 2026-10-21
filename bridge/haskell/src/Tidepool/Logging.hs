-- | Opt-in structured diagnostics for the compiler worker.
--
-- One JSON event is written per line on stderr. The @tidepool-log @ prefix is
-- deliberately distinct from timing, reuse, memo, retention and protocol
-- records, whose grammars have independent consumers. Diagnostic delivery is
-- best effort: a closed or otherwise unavailable stderr must not replace the
-- compiler result, while asynchronous cancellation still escapes.
module Tidepool.Logging
  ( LogLevel(..)
  , Logger
  , LogField
  , LogValue(..)
  , logLevelFromName
  , readLogger
  , newLogger
  , loggerEnabled
  , logEvent
  , logEventTo
  , logTrace
  , renderLogEvent
  ) where

import Control.Monad.IO.Class (MonadIO, liftIO)
import Data.Char (toLower)
import Data.List (intercalate)
import Data.Word (Word64)
import GHC.Clock (getMonotonicTimeNSec)
import System.Environment (lookupEnv)
import System.IO (Handle, hPutStrLn, stderr)
import Tidepool.ExtractUtil (trySynchronous)
import Tidepool.Json (jsonString)

-- | Increasing verbosity. 'LogOff' disables every call site.
data LogLevel = LogOff | LogError | LogWarn | LogInfo | LogDebug | LogTrace
  deriving (Eq, Ord, Show)

-- | Parsed once by an owning operation. Keeping the target and threshold in a
-- value avoids repeated environment reads and lets callers reject a logging
-- failure without changing the primary compiler result.
data Logger = Logger
  { loggerTarget :: !String
  , loggerMinimum :: !LogLevel
  }

type LogField = (String, LogValue)

data LogValue
  = LogText String
  | LogInteger Integer
  | LogBool Bool
  | LogNull
  deriving (Eq, Show)

-- | Invalid or absent values disable logging. The accepted spelling is the
-- level name case-insensitively; there is no separate boolean convention.
logLevelFromName :: String -> LogLevel
logLevelFromName value = case map toLower value of
  "off" -> LogOff
  "error" -> LogError
  "warn" -> LogWarn
  "info" -> LogInfo
  "debug" -> LogDebug
  "trace" -> LogTrace
  _ -> LogOff

readLogger :: String -> IO Logger
readLogger target = do
  configured <- lookupEnv "TIDEPOOL_LOG"
  pure (newLogger target (maybe LogOff logLevelFromName configured))

newLogger :: String -> LogLevel -> Logger
newLogger = Logger

loggerEnabled :: Logger -> LogLevel -> Bool
loggerEnabled logger level =
  level /= LogOff && level <= loggerMinimum logger
{-# INLINE loggerEnabled #-}

-- | Render one bounded structured event. Exporting the exact renderer keeps
-- tests on the production encoding instead of duplicating JSON rules.
renderLogEvent :: Word64 -> LogLevel -> String -> String -> [LogField] -> String
renderLogEvent observed loggerLevel target event fields =
  "tidepool-log {" ++ intercalate ","
    [ "\"schema\":1"
    , "\"level\":" ++ jsonString (levelName loggerLevel)
    , "\"target\":" ++ jsonString (boundedText 256 target)
    , "\"event\":" ++ jsonString (boundedText 256 event)
    , "\"observed_ns\":" ++ show observed
    , "\"fields\":{" ++ intercalate "," (renderField <$> boundedFields fields) ++ "}"
    ] ++ "}"

-- | Emit through the process diagnostic channel. The level check precedes the
-- clock read, rendering and exception boundary, so disabled trace sites are
-- cheap and observably silent.
logEvent :: MonadIO m => Logger -> LogLevel -> String -> [LogField] -> m ()
logEvent logger level event fields =
  whenEnabled logger level $ liftIO $ emit stderr logger level event fields

-- | Handle-directed form used by focused failure tests and any future owned
-- sink. Ordinary compiler code uses 'logEvent'.
logEventTo :: MonadIO m => Handle -> Logger -> LogLevel -> String -> [LogField] -> m ()
logEventTo handle logger level event fields =
  whenEnabled logger level $ liftIO $ emit handle logger level event fields

logTrace :: MonadIO m => Logger -> String -> [LogField] -> m ()
logTrace logger = logEvent logger LogTrace

whenEnabled :: MonadIO m => Logger -> LogLevel -> m () -> m ()
whenEnabled logger level action
  | loggerEnabled logger level = action
  | otherwise = pure ()

emit :: Handle -> Logger -> LogLevel -> String -> [LogField] -> IO ()
emit handle logger level event fields = do
  _ <- trySynchronous $ do
    observed <- getMonotonicTimeNSec
    hPutStrLn handle (renderLogEvent observed level (loggerTarget logger) event fields)
  pure ()

renderField :: LogField -> String
renderField (name, value) =
  jsonString (boundedText 128 name) ++ ":" ++ renderValue value

renderValue :: LogValue -> String
renderValue value = case value of
  LogText text -> jsonString (boundedText 4096 text)
  LogInteger number -> show number
  LogBool True -> "true"
  LogBool False -> "false"
  LogNull -> "null"

-- Repeated names would make a JSON object ambiguous. The first field wins;
-- call sites should carry distinct keys and treat omission as a programming
-- error rather than a reason to fail the compiler operation.
boundedFields :: [LogField] -> [LogField]
boundedFields = go []
  where
    go _ [] = []
    go seen ((name, value):rest)
      | normalized `elem` seen = go seen rest
      | otherwise = (normalized, value) : go (normalized : seen) rest
      where
        normalized = boundedText 128 name

boundedText :: Int -> String -> String
boundedText limit text =
  let (prefix, suffix) = splitAt limit text
  in prefix ++ if null suffix then "" else "…"

levelName :: LogLevel -> String
levelName level = case level of
  LogOff -> "off"
  LogError -> "error"
  LogWarn -> "warn"
  LogInfo -> "info"
  LogDebug -> "debug"
  LogTrace -> "trace"
