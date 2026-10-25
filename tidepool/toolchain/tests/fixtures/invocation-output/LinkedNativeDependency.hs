module LinkedNativeDependency where

import Data.List (reverse)

{-# NOINLINE depReverse #-}
depReverse :: [Int] -> [Int]
depReverse = reverse

{-# NOINLINE depIdentity #-}
depIdentity :: [Int] -> [Int]
depIdentity xs = xs
