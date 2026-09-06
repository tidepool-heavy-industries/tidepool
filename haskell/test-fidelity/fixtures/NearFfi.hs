{-# LANGUAGE ForeignFunctionInterface #-}
module NearFfi where
import Foreign.C.Types
foreign import ccall unsafe "user_isDoubleNaN_suffix" near :: CDouble -> CInt
probe :: Double -> Int
probe x = fromIntegral (near (CDouble x))
