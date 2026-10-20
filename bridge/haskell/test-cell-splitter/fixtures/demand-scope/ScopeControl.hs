{-# LANGUAGE GHC2024, NamedDefaults #-}
module Probe where
import Prelude
import qualified Data.List as L
data Box = Box { field :: Int }
class C a where
  method :: a -> Int
  method argument = old + field (Box 1)
instance C Int where
  method argument = let old = argument in old
helper = id old
__tidepool_segment_check = do
  let old = 1
  pure (old, L.length [1], field (Box 1))
