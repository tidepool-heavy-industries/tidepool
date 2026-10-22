module TransitiveHomeMiddle where

import TransitiveHomeLeaf

{-# NOINLINE middleValue #-}
middleValue :: Int
middleValue = leafValue + 1
