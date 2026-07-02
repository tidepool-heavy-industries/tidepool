{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
-- | Repo-history verbs: recency-weighted churn analysis over the Git effect.
module Churn where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Data.Map.Strict as Map
import qualified Data.List as L
import qualified Data.Text as T

-- | Inverse of Tidepool.Data.Time's civilFromDays (Hinnant days_from_civil):
-- (year, month, day) -> days since 1970-01-01. Pure Int arithmetic.
daysFromCivil :: Int -> Int -> Int -> Int
daysFromCivil y0 m d =
  let y   = y0 - (if m <= 2 then 1 else 0)
      era = y `div` 400
      yoe = y - era * 400
      doy = (153 * (m + (if m > 2 then -3 else 9)) + 2) `div` 5 + d - 1
      doe = yoe * 365 + yoe `div` 4 - yoe `div` 100 + doy
  in era * 146097 + doe - 719468

-- | Parse an ISO-8601 timestamp with numeric offset or trailing Z (git's
-- %cI shape, e.g. "2026-07-01T19:24:22-07:00") into a UTCTime.
-- Fixed-width slicing; inverse of formatISO8601 for the Z form.
parseISO8601 :: Text -> UTCTime
parseISO8601 t =
  let grab a n = parseInt (T.take n (T.drop a t))
      days = daysFromCivil (grab 0 4) (grab 5 2) (grab 8 2)
      secs = days * 86400 + grab 11 2 * 3600 + grab 14 2 * 60 + grab 17 2
      off  = case T.uncons (T.drop 19 t) of
               Just ('+', _) -> grab 20 2 * 3600 + grab 23 2 * 60
               Just ('-', _) -> negate (grab 20 2 * 3600 + grab 23 2 * 60)
               _             -> 0
  in UTCTime ((secs - off) * 1000)

-- | Heat of one commit at `now`: 0.5 ** (ageDays / halfLifeDays).
commitHeat :: UTCTime -> Double -> Commit -> Double
commitHeat now halfLife c =
  0.5 ** (diffUTCTime now (parseISO8601 c.date) / 86400 / halfLife)

-- | Recency-weighted churn hotspots: fold the last n commits' file lists
-- with a 30-day half-life; top k (path, heat) pairs, hottest first.
-- Example: hotspots 300 12
hotspots :: Int -> Int -> M [(Text, Double)]
hotspots n k = do
  cs  <- gitLog n
  now <- getCurrentTime
  let hot = Map.fromListWith (+) [(f, commitHeat now 30 c) | c <- cs, f <- c.files]
  pure (L.take k (L.sortOn (negate . snd) (Map.toList hot)))
