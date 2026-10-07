boxed <- pure (SigmaNumber 7)
intValue <- pure (sigmaNumber boxed + 2 :: Int)
doubleValue <- pure (if (sigmaNumber boxed + 3 :: Double) == 10 then (11 :: Int) else 0)
