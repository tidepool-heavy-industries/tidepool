module CandidateDemandClass where

class Available a where
  available :: a -> Int

{-# NOINLINE classAnchor #-}
classAnchor :: Int
classAnchor = 1
