capturedForNative <- pure (23 :: Int)
{-# NOINLINE privateOriginalCapture #-}
privateOriginalCapture :: Int
privateOriginalCapture = capturedForNative
nativeResult <- pure privateOriginalCapture
