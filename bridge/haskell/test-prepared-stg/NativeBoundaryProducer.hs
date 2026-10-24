module NativeBoundaryProducer
  ( alias, inlineAlias, poly, strictPair, Single(..), Multi(..), Box(..), Wrap(..) ) where

import NativeBoundarySource (seed, sourceId)

alias :: Int
alias = seed

inlineAlias :: Int -> Int
inlineAlias value = seed + value
{-# INLINE inlineAlias #-}

poly :: Num value => value -> value
poly value = value + fromIntegral seed
{-# SPECIALIZE poly :: Int -> Int #-}

strictPair :: (Int, Int) -> (Int, Int)
strictPair (left, right) = (left + seed, right + 1)

class Single value where
  single :: value -> Int
  single _ = seed

class Multi value where
  first :: value -> Int
  second :: value -> Int
  first value = seed + second value

data Box = Box { unBox :: Int }
newtype Wrap = Wrap { unWrap :: Int }

{-# RULES "native-imported-head" forall value. sourceId value = seed #-}
