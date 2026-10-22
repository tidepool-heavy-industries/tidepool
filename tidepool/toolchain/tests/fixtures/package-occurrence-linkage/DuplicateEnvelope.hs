-- P3 ordinary smoke fixture reached under two different subsets of Data.List.
-- Current compiler output does not produce one envelope under two different
-- import histories, so this fixture does not claim that relationship.
module DuplicateEnvelope where

import Data.List (reverse, sort)

{-# NOINLINE firstHistory #-}
firstHistory :: [Int] -> [Int]
firstHistory = reverse

{-# NOINLINE secondHistory #-}
secondHistory :: [Int] -> [Int]
secondHistory = reverse . sort
