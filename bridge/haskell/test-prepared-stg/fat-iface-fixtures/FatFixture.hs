module FatFixture (fatIdentity, recA, recB, privateCaller, privateSecond, privateDiamond) where

fatIdentity :: Int -> Int
fatIdentity value = value
{-# NOINLINE fatIdentity #-}

recA :: Int -> Int
recA value = recB value
{-# NOINLINE recA #-}

recB :: Int -> Int
recB value = recA value
{-# NOINLINE recB #-}

privateHelper :: Int -> Int
privateHelper value = value + 1
{-# NOINLINE privateHelper #-}

privateCaller :: Int -> Int
privateCaller value = privateHelper value
{-# NOINLINE privateCaller #-}

privateSecond :: Int -> Int
privateSecond value = privateHelper (value + 1)
{-# NOINLINE privateSecond #-}

privateDiamond :: Int -> Int
privateDiamond value = privateCaller value + privateSecond value
{-# NOINLINE privateDiamond #-}
