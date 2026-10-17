module Project.OrdinaryDemandProbe (ordinaryEntry, ordinaryLater, unusedA, unusedB) where

{-# NOINLINE ordinaryEntry #-}
ordinaryEntry :: Int
ordinaryEntry = 42

{-# NOINLINE ordinaryLater #-}
ordinaryLater :: Int
ordinaryLater = 73

{-# NOINLINE unusedA #-}
unusedA :: Int -> Int
unusedA x = x + 19

{-# NOINLINE unusedB #-}
unusedB :: Int -> Int
unusedB x = x * 23
