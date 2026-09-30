{-# LANGUAGE TypeFamilies #-}
module CacheEven where
import Prelude

data Parity = Parity Bool
instance Eq Parity
type family Payload a
even' :: Int -> Parity
