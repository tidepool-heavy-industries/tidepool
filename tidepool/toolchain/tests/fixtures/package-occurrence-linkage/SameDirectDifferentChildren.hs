-- Two targets share the direct sort implementation and pass different Ord
-- dictionaries as siblings. Unrelated target roots can change receipt cohorts
-- while the shared package body keeps the same closed code identity.
module SameDirectDifferentChildren where

import Data.List (sort)

{-# NOINLINE sortInts #-}
sortInts :: [Int] -> [Int]
sortInts = sort

{-# NOINLINE sortStrings #-}
sortStrings :: [String] -> [String]
sortStrings = sort
