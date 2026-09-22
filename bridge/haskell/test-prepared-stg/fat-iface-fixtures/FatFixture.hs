module FatFixture where

fatIdentity :: Int -> Int
fatIdentity value = value
{-# NOINLINE fatIdentity #-}

recA :: Int -> Int
recA value = recB value
{-# NOINLINE recA #-}

recB :: Int -> Int
recB value = recA value
{-# NOINLINE recB #-}
