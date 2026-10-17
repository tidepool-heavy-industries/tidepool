module OrdinaryDemandProbe (entry, later, unusedA, unusedB) where

{-# NOINLINE entry #-}
entry :: Int
entry = 42

{-# NOINLINE later #-}
later :: Int
later = 73

{-# NOINLINE unusedA #-}
unusedA :: Int -> Int
unusedA x = x + 19

{-# NOINLINE unusedB #-}
unusedB :: Int -> Int
unusedB x = x * 23
