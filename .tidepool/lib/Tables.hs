{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Tables where

import Tidepool.Prelude

-- | Render (label, count) pairs as an aligned two-column table.
countTable :: [(Text, Int)] -> Text
countTable rows =
  let w = foldl' (\m (k, _) -> max' m (len k)) 0 rows
      pad t = t <> pack (replicate (w - len t) ' ')
  in unlines (map (\(k, n) -> pad k <> "  " <> pack (show n)) rows)

