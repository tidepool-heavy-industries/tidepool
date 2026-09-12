module M3Vertical where

import Data.List (foldl')

data Box = Box !Int

result :: Int
result = 42

entry :: Int -> Box
entry count = go count (foldl' (+) 0 [1, 2, 3])
  where
    go 0 value = Box value
    go remaining value = go (remaining - 1) (value + 1)
