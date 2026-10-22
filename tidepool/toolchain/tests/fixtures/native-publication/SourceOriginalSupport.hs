module SourceOriginalSupport (first, second) where

{-# NOINLINE first #-}
first :: Int
first = 17

{-# NOINLINE second #-}
second :: Int
second = 23
