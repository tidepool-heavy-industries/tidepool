module WorldPackageRuntime where

import Data.List (reverse)

{-# NOINLINE homeWorker #-}
homeWorker :: [Int] -> [Int]
homeWorker = reverse

{-# NOINLINE first #-}
first :: [Int] -> [Int]
first xs = homeWorker (reverse xs)
