data CapturedReuseValue = CapturedReuseValue Int
reusedCapturedValue <- pure (CapturedReuseValue (privateCapturedHelper capturedValue))
respond (case reusedCapturedValue of CapturedReuseValue original -> original)
