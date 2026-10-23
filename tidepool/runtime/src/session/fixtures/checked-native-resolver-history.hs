capturedForNative <- pure (23 :: Int)
{-# NOINLINE firstNativeCapture #-}
firstNativeCapture :: Int
firstNativeCapture = capturedForNative + 1
{-# NOINLINE secondNativeCapture #-}
secondNativeCapture :: Int
secondNativeCapture = capturedForNative + 2
{-# NOINLINE nativeCaptureParent #-}
nativeCaptureParent :: Int
nativeCaptureParent = firstNativeCapture + secondNativeCapture
firstNativeResult <- pure nativeCaptureParent
secondNativeResult <- pure nativeCaptureParent
