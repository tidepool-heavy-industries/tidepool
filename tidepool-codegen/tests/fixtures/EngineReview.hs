module EngineReview where

import Data.Char (ord)
import Prelude hiding ((++))

{-# OPAQUE keep #-}
keep :: Int -> Int -> Int
keep x _ = x

{-# OPAQUE bad #-}
bad :: Int -> Int
bad x = 10 `quot` x

lazyArgument :: Int
lazyArgument = keep 42 (bad 0)

{-# OPAQUE loop #-}
loop :: Int -> Int
loop n = loop (n + 1) + n

unusedLoop :: Int
unusedLoop = keep 42 (loop 0)

{-# OPAQUE (++) #-}
(++) :: [Int] -> [Int] -> [Int]
xs ++ _ = xs

customAppend :: Int
customAppend = length ([1] ++ [2])

{-# OPAQUE passString #-}
passString :: String -> String
passString x = x

nulString :: Int
nulString = sum (map ord (passString "a\0b"))
