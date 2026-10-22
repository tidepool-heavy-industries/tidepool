module LinkedNativeTargets where

import Data.List (reverse)
import qualified LinkedNativeDependency

{-# NOINLINE first #-}
first :: [Int] -> [Int]
first = reverse

{-# NOINLINE second #-}
second :: [Int] -> [Int]
second = LinkedNativeDependency.depReverse
