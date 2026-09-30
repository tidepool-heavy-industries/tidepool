{-# LANGUAGE MultiParamTypeClasses, TypeFamilies, FlexibleInstances #-}
module AmbiguousInventory where
class Partial a b where
  type PartialFamily a
instance Partial Int Bool where
  type PartialFamily Int = Int
instance Partial Int Char
