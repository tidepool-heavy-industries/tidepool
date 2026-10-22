module WorldPackageRuntime where

import Data.List (reverse)

{-# NOINLINE first #-}
first :: [Int] -> [Int]
first = reverse
