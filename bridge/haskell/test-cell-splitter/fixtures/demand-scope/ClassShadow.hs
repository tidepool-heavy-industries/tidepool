{-# LANGUAGE GHC2024 #-}
module ClassShadow where
import Prelude
class C a where
  old :: a -> Int
  old argument = helper argument
instance C Int where
  old argument = argument
helper x = old x
root = helper 1
