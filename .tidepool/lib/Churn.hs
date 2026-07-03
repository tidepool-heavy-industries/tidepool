{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
-- | Repo-history verbs: recency-weighted churn analysis over the Git effect.
--
-- 'parseISO8601' and 'daysFromCivil' now live in Tidepool.Data.Time and are
-- re-exported by Tidepool.Prelude; the local copies have been removed.
module Churn where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Data.Map.Strict as Map
import qualified Data.List as L
import qualified Data.Text as T

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
