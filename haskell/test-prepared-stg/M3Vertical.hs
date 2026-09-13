{-# LANGUAGE MagicHash #-}
{-# LANGUAGE UnboxedTuples #-}

module M3Vertical where

import Data.List (foldl')
import GHC.Exts (Double#, Int#, Int(I#), double2Int#, (+#))

data Box = Box !Int

result :: Int
result = 42

tupleArgumentCall :: (# Int#, Double# #) -> Int#
tupleArgumentCall (# count, ratio #) = count +# double2Int# ratio
{-# NOINLINE tupleArgumentCall #-}

tupleArgumentUse :: (# Int#, Double# #) -> Int
tupleArgumentUse (# count, ratio #) = I# (tupleArgumentCall (# count, ratio #))

tupleArgumentResult :: Int
tupleArgumentResult = tupleArgumentUse (# 41#, 1.0## #)

entry :: Int -> Box
entry count = go count (foldl' (+) 0 [1, 2, 3])
  where
    go 0 value = Box value
    go remaining value = go (remaining - 1) (value + 1)
