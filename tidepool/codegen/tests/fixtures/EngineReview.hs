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

data LazyPair = LazyPair Int Int

lazyConstructor :: Int
lazyConstructor = case LazyPair 42 (bad 0) of
  LazyPair first _ -> first

data FunctionBox = FunctionBox (Int -> Int)

{-# OPAQUE makeAdderBox #-}
makeAdderBox :: Int -> FunctionBox
makeAdderBox x = FunctionBox (x +)

returnedFunction :: Int
returnedFunction = case makeAdderBox 1 of
  FunctionBox function -> function 41

{-# OPAQUE countDown #-}
countDown :: Int -> Int -> Int -> Int
countDown 0 step total = total + step
countDown n step total = countDown (n - 1) step (total + 1)

multiParameterRecursion :: Int
multiParameterRecursion = countDown 100 1 0

multibyteChars :: Int
multibyteChars = sum (map ord (passString "λ🙂"))

data StrictBox = StrictBox !Int

strictFieldFailure :: Int
strictFieldFailure = StrictBox (bad 0) `seq` 42
