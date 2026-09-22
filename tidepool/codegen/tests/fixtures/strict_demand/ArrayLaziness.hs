{-# LANGUAGE MagicHash #-}
{-# LANGUAGE UnboxedTuples #-}
module Main (main) where

import Control.Exception (ErrorCall, evaluate, try)
import GHC.Exts (Int(I#), newSmallArray#, readSmallArray#, runRW#)

-- Demand array creation/state sequencing, but not the lifted initializer.
created :: Int
created = runRW# (\s ->
  case newSmallArray# 1# (error "lazy initializer" :: Int) s of
    (# _, _ #) -> I# 42#)

-- Reading returns a lifted element; evaluate below demands that element.
selected :: Int
selected = runRW# (\s ->
  case newSmallArray# 1# (error "lazy initializer" :: Int) s of
    (# s', array #) -> case readSmallArray# array 0# s' of
      (# _, value #) -> value)

main :: IO ()
main = do
  value <- evaluate created
  if value == 42 then putStrLn "created: 42" else error "wrong creation result"
  outcome <- try (evaluate selected) :: IO (Either ErrorCall Int)
  case outcome of
    Left _ -> putStrLn "selected: ErrorCall"
    Right _ -> error "selected initializer unexpectedly succeeded"
