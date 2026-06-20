-- | Phase-2 test helper: home-module bindings that DELEGATE to the vendored
-- Tidepool.Data.Text (mirrors how a Prelude shadow / .tidepool/lib verb / user
-- helper wraps a T.* op). Delegating to a vendored HOME body is the proven-green
-- pattern; delegating to EXTERNAL Data.Text is the red one.
module Tidepool.DataTextWrap where

import qualified Tidepool.Data.Text as T
import Data.Text (Text)

twW, dwW, twEndW, dwEndW, daW :: (Char -> Bool) -> Text -> Text
twW    = T.takeWhile
dwW    = T.dropWhile
twEndW = T.takeWhileEnd
dwEndW = T.dropWhileEnd
daW    = T.dropAround

filterW :: (Char -> Bool) -> Text -> Text
filterW = T.filter

spanW, breakW :: (Char -> Bool) -> Text -> (Text, Text)
spanW  = T.span
breakW = T.break

partitionW :: (Char -> Bool) -> Text -> (Text, Text)
partitionW = T.partition

allW, anyW :: (Char -> Bool) -> Text -> Bool
allW = T.all
anyW = T.any

findW :: (Char -> Bool) -> Text -> Maybe Char
findW = T.find

findIndexW :: (Char -> Bool) -> Text -> Maybe Int
findIndexW = T.findIndex

splitW :: (Char -> Bool) -> Text -> [Text]
splitW = T.split

groupByW :: (Char -> Char -> Bool) -> Text -> [Text]
groupByW = T.groupBy
