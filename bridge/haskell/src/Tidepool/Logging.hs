-- | Opt-in structured diagnostics for the compiler worker.
--
-- One JSON event is written per line on stderr. The @tidepool-log @ prefix is
-- deliberately distinct from timing, reuse, memo, retention and protocol
-- records, whose grammars have independent consumers. Diagnostic delivery is
-- best effort: a closed or otherwise unavailable stderr must not replace the
-- compiler result, while asynchronous cancellation still escapes.
--
-- /Bounded events./ Every value is drawn from a finite vocabulary: 'LogText'
-- is capped at 4096 characters, field names at 128, the target and event at
-- 256, and 'LogInteger' carries an 'Int64'. Each event also renders at most
-- 'maxFields' distinct caller fields. The field cap is diagnostic payload
-- control: production events are trace records with small static field lists
-- (the largest current site, @finalized.owner@, has twelve distinct fields),
-- and the cap stops a future call site from growing a diagnostic line without
-- bound. When the cap drops distinct fields the event records an explicit
-- @omitted_fields@ count instead of silently losing them.
--
-- /Total encoding bound./ @jsonString@'s worst case is a control character
-- rendered as @\\u00XX@, six bytes, so a bounded string of @n@ characters
-- contributes at most @2 + 6*n@ bytes and an 'Int64'/'Word64' at most twenty
-- decimal digits. The rendered event is therefore at most the sum of
--
-- @
--   "tidepool-log {"                                  14
--   "schema":1                                        10
--   "level":"trace"                                   15
--   "target":2 + 6*256                              1547
--   "event":2 + 6*256                               1546
--   "observed_ns":20                                  34
--   "fields":{32*(2+6*128 + 1 + 2+6*4096) + 31}    811210
--   "omitted_fields":20 (only when capped)            37
--   member separators and closing brace                21
-- @
--
-- i.e. about 814420 bytes (~795 KiB) in the absolute worst case, with the
-- omission marker included. This is a static ceiling that follows from finite
-- inputs, so no per-event byte budget manager is needed for the current
-- workload.
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
  ) where

import Control.Monad.IO.Class (MonadIO, liftIO)
import Data.Char (toLower)
import Data.Int (Int64)
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
  | LogInteger Int64
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

-- | Fix a target and threshold without re-reading the environment. This is the
-- shared compiler-internal constructor behind 'readLogger'; production call
-- sites go through 'readLogger' while tests build explicit levels directly.
newLogger :: String -> LogLevel -> Logger
newLogger = Logger

loggerEnabled :: Logger -> LogLevel -> Bool
loggerEnabled logger level =
  level /= LogOff && level <= loggerMinimum logger
{-# INLINE loggerEnabled #-}

-- | Render one bounded structured event. Internal to the emission path so
-- tests observe the actual sink rather than a duplicated JSON rule.
renderLogEvent :: Word64 -> LogLevel -> String -> String -> [LogField] -> String
renderLogEvent observed loggerLevel target event fields =
  let (kept, omitted) = boundedFields fields
      members =
        [ "\"schema\":1"
        , "\"level\":" ++ jsonString (levelName loggerLevel)
        , "\"target\":" ++ jsonString (boundedText 256 target)
        , "\"event\":" ++ jsonString (boundedText 256 event)
        , "\"observed_ns\":" ++ show observed
        , "\"fields\":{" ++ intercalate "," (renderField <$> kept) ++ "}"
        ] ++ [ "\"omitted_fields\":" ++ show omitted | omitted > 0 ]
  in "tidepool-log {" ++ intercalate "," members ++ "}"

-- | Emit through the process diagnostic channel. The level check precedes the
-- clock read, rendering and exception boundary, so disabled trace sites are
-- cheap and observably silent.
logEvent :: MonadIO m => Logger -> LogLevel -> String -> [LogField] -> m ()
logEvent logger level event fields =
  logEventTo stderr logger level event fields

-- | Handle-directed form of the emission boundary. 'logEvent' supplies the
-- production stderr sink; focused tests supply a scratch handle so they can
-- observe real emission and ordinary sink failure without duplicating the
-- renderer.
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
-- error rather than a reason to fail the compiler operation. Distinct fields
-- beyond 'maxFields' are dropped, and the returned count makes that omission
-- explicit at the event boundary.
boundedFields :: [LogField] -> ([LogField], Int)
boundedFields fields =
  let normalized = [(boundedText 128 name, value) | (name, value) <- fields]
      (kept, dropped) = splitAt maxFields (dedupe normalized)
  in (kept, length dropped)
  where
    dedupe = go []
      where
        go _ [] = []
        go seen (field@(name, _):rest)
          | name `elem` seen = go seen rest
          | otherwise = field : go (name : seen) rest

-- | The most distinct caller fields one event renders. Production events are
-- small (twelve fields at the largest current site); this caps pathological
-- growth while leaving headroom for the current workload.
maxFields :: Int
maxFields = 32

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
