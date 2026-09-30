{-# LANGUAGE MultiParamTypeClasses, TypeFamilies, FlexibleInstances, NoPolyKinds #-}
module RichInventory where
class Rich a where
  type Extra a b
  type Default a
  type Default a = [a]
  data RichData a
instance Rich [a] where
  type Extra [a] b = (a, b)
  data RichData [a] = RichList a
class Permuted a b where
  type Reverse b a
instance Permuted Int Bool where
  type Reverse Bool Int = Char
