{-# LANGUAGE TypeFamilies #-}
module CacheEven where
import Prelude
import qualified CacheOdd

data Parity = Parity Bool deriving Eq
type family Payload a
type instance Payload Bool = Bool

even' :: Int -> Parity
even' n = Parity (n == 0 || CacheOdd.odd' (n - 1))
