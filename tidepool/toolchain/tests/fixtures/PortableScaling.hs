module PortableScaling where

import Data.List (foldl', reverse)
import PortableSupport (support)

data Box = Box !Int

packageValue :: Int -> Int
packageValue n = foldl' (+) 0 (reverse [1,n])
{-# NOINLINE packageValue #-}

boxed :: Int -> Box
boxed n = Box (packageValue (support n))
{-# NOINLINE boxed #-}

first :: Int
first = case boxed 7 of Box value -> value

second :: Int
second = case boxed 8 of Box value -> value

-- UNRELATED_EXPORTS
