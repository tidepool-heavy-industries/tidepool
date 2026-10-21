module CanonicalPartialConsumer (usePartial) where

import qualified CanonicalPartial

{-# NOINLINE usePartial #-}
usePartial :: Int -> Int
usePartial value = CanonicalPartial.partial (Just value)
