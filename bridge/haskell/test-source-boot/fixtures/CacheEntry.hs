{-# LANGUAGE TypeFamilies #-}
module CacheEntry where
import Prelude
import qualified CacheEven

payload :: CacheEven.Payload Bool -> Bool
payload value = value

result :: Bool
result = payload True && CacheEven.even' 42 == CacheEven.Parity True
