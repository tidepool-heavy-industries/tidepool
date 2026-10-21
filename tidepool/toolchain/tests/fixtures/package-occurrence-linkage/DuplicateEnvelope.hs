-- P3 oracle fixture 4: one defining module reached under two different child
-- histories. Both targets import Data.List symbols, but their reachability
-- differs, so the retained package slots are distinct even when envelope bytes
-- are shared. A manifest keyed by envelope SHA would collapse them; an
-- OccurrenceId keeps each history a distinct binding.
module DuplicateEnvelope where

import Data.List (reverse, sort)

{-# NOINLINE firstHistory #-}
firstHistory :: [Int] -> [Int]
firstHistory = reverse

{-# NOINLINE secondHistory #-}
secondHistory :: [Int] -> [Int]
secondHistory = reverse . sort
