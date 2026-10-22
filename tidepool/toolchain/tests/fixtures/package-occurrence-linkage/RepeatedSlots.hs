-- P3 physical-dedup fixture: GHC deduplicates the three authored `sort`
-- demands into one package global. This fixture does not establish receipt
-- slot multiplicity; repeated wire roots are covered by the HOME target/root
-- controls.
module RepeatedSlots where

import Data.List (sort)

{-# NOINLINE threeSorts #-}
threeSorts :: ([Int], [Int], [Int]) -> ([Int], [Int], [Int])
threeSorts (a, b, c) = (sort a, sort b, sort c)
