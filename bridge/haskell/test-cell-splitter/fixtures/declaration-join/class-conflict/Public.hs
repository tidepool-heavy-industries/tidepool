module Public (C(..)) where

class C a where
  publicMethod :: a -> Int
