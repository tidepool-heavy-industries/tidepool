module CacheOdd where
import Prelude
import {-# SOURCE #-} qualified CacheEven

odd' :: Int -> Bool
odd' n = n /= 0 && CacheEven.even' (n - 1) == CacheEven.Parity True
