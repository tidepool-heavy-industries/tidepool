-- Two compiler histories share the direct sort implementation but select
-- different Ord dictionaries. Their serialized program/package pairs must not
-- be interchangeable even when the direct package identity is equal.
module SameDirectDifferentChildren where

import Data.List (sort)

{-# NOINLINE sortInts #-}
sortInts :: [Int] -> [Int]
sortInts = sort

{-# NOINLINE sortStrings #-}
sortStrings :: [String] -> [String]
sortStrings = sort
