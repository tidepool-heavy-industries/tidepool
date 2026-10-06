{-# LANGUAGE DataKinds, TemplateHaskell #-}
module NativeEpochTarget where

import Data.Proxy (Proxy(..))
import NativeEpochProvider (nativeType)

__result :: Proxy $(nativeType)
__result = Proxy
