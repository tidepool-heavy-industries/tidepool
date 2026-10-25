module PublicationHelper (first, second, Witness) where

data Witness = Witness

{-# NOINLINE first #-}
first :: Int
first = 17

{-# NOINLINE second #-}
second :: Int
second = 23
