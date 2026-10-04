data CapturedReuseValue = CapturedReuseValue Int
reusedCapturedValue <- pure (CapturedReuseValue capturedGetter)
respond (case reusedCapturedValue of CapturedReuseValue original -> original)
