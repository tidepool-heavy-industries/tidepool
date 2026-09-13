module FatFixture where

fatIdentity :: Int -> Int
fatIdentity value = value
{-# NOINLINE fatIdentity #-}

recA :: Int -> Int
recA value = if value == 0 then 0 else recB (value - 1)
{-# NOINLINE recA #-}

recB :: Int -> Int
recB value = if value == 0 then 1 else recA (value - 1)
{-# NOINLINE recB #-}
