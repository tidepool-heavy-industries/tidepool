module NativePackageTargets where

import Data.List (reverse)

{-# NOINLINE first #-}
first :: [Int] -> [Int]
first = reverse

{-# NOINLINE second #-}
second :: [Int] -> Int
second xs = length (first xs)
