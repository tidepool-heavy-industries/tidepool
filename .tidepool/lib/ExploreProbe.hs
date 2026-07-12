{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
module ExploreProbe where

import Tidepool.Prelude
import Tidepool.Effects

biggestRs :: Int -> M [(Text, Int)]
biggestRs n = do
  Right fs <- glob "**/*.rs"
  sizes <- mapM (\f -> fsMeta f <&> \mm -> (f, case mm of { Just m -> m.size; Nothing -> 0 })) fs
  pure (take n (sortBy (\a b -> compare (snd b) (snd a)) sizes))
