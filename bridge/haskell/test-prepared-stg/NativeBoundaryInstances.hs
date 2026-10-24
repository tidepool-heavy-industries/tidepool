module NativeBoundaryInstances () where

import NativeBoundaryProducer

instance Single Bool

instance Multi [value] where
  second _ = 1
  {-# SPECIALIZE instance Multi [Bool] #-}
