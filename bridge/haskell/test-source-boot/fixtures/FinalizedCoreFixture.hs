module FinalizedCoreFixture (Box(..), HasBox(..), identity, evenBox) where

data Box a = Box a

class HasBox a where
  box :: a -> Box a

instance HasBox Int where
  box = Box

{-# NOINLINE identity #-}
identity :: a -> a
identity value = value

{-# NOINLINE evenBox #-}
evenBox :: Int -> Box Int
evenBox value
  | value <= 0 = box (identity value)
  | otherwise = oddBox (value - 1)

{-# NOINLINE oddBox #-}
oddBox :: Int -> Box Int
oddBox value
  | value <= 0 = box (privateHelper value)
  | otherwise = evenBox (value - 1)

{-# NOINLINE privateHelper #-}
privateHelper :: Int -> Int
privateHelper value = if value >= 0 then value + 1 else value - 1
