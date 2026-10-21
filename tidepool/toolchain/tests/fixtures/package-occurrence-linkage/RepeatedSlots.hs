-- P3 oracle fixture 3: repeated import slots over shared identical leaves.
--
-- `threeSorts` demands the same package symbol three times in one program
-- over three distinct arguments, so the target keeps three ordered slots over
-- one shared immutable body. Byte sharing must not erase slot multiplicity.
module RepeatedSlots where

import Data.List (sort)

{-# NOINLINE threeSorts #-}
threeSorts :: ([Int], [Int], [Int]) -> ([Int], [Int], [Int])
threeSorts (a, b, c) = (sort a, sort b, sort c)
