{-# LANGUAGE GHC2024 #-}
module ImportedMethod where
import Prelude
data Box = Box { field :: Int }
instance Show Box where
  show box = old ++ let old = "local" in old
root = field (Box 1)
