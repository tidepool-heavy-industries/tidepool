module Private (C(..)) where

class C a where
  privateMethod :: a -> Int
