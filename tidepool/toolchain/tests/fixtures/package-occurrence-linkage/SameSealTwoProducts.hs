-- P3 oracle fixture 1: two targets import the same package symbol while
-- reaching different subsets of one defining module.
--
-- `onlySort` reaches GHC.Internal.Data.OldList.sort alone;
-- `sortThenUnlines` reaches sort and unlines. Both targets import the same
-- package symbol (sort) under one defining-interface seal, but the retained
-- bodies of GHC.Internal.Data.OldList differ. A (seal, symbol) selector cannot
-- tell the two bodies apart; an exact occurrence link can.
module SameSealTwoProducts where

import Data.List (sort, unlines)

{-# NOINLINE onlySort #-}
onlySort :: [Int] -> [Int]
onlySort = sort

{-# NOINLINE sortThenUnlines #-}
sortThenUnlines :: [String] -> String
sortThenUnlines = unlines . sort
