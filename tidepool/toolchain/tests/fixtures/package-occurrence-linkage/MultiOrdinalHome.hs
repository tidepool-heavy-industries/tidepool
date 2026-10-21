-- P3 oracle fixture 5: one fresh HOME module with two original ordinals, each
-- importing package roots in a different order.
--
-- `byList` (ordinal 0) imports Data.List.sort; `byText` (ordinal 1) imports
-- Data.Text.pack. A manifest keyed only by (unit, module) would merge the two
-- groups' package roots and force a shared cursor to consume them in the wrong
-- order. Keying by (unit, module, original ordinal) keeps each group's roots
-- with its own group.
module MultiOrdinalHome where

import Data.List (sort)
import Data.Text (Text, pack)

{-# NOINLINE byList #-}
byList :: [Int] -> [Int]
byList = sort

{-# NOINLINE byText #-}
byText :: String -> Text
byText = pack
