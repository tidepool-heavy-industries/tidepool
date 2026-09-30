{-# LANGUAGE GADTs, TypeFamilies, FlexibleInstances #-}
data Box where
  Box :: Int -> Box
type family Payload a
type instance Payload Bool = Int
class Tagged a where
  tag :: a -> Int
instance Tagged Box where
  tag (Box value) = value
let value = Box 42
tag value
