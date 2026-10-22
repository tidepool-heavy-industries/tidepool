module LinkedNativeDependency where

import Data.List (reverse)

{-# NOINLINE depReverse #-}
depReverse :: [Int] -> [Int]
depReverse = reverse
