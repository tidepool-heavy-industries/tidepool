module TransitiveHomeTarget where

import TransitiveHomeMiddle

{-# NOINLINE target #-}
target :: Int
target = middleValue
