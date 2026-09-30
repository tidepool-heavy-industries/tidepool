{-# LANGUAGE TypeFamilies, NoPolyKinds #-}
module Tidepool.Session.Lib.G1 (Box(..), Choice(..), answer) where
data Box = Box Int
class Choice a where
  type Chosen a
  choose :: a -> Int
instance Choice Int where
  type Chosen Int = Bool
  choose _ = 42
type family Standalone a
type instance Standalone Char = Int
answer :: Int
answer = choose (0 :: Int)
