{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Tabular rendering utilities: aligned text tables from key/count pairs.
module Tables where

import Tidepool.Prelude
import qualified Tidepool.Data.Text as T

-- | Render (label, count) pairs as an aligned two-column table.
countTable :: [(Text, Int)] -> Text
countTable rows =
  let w = foldl' (\m (k, _) -> max m (T.length k)) 0 rows
      pad t = t <> pack (replicate (w - T.length t) ' ')
  in unlines (map (\(k, n) -> pad k <> "  " <> pack (show n)) rows)

