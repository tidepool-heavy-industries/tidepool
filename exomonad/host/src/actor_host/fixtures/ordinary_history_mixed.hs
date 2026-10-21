ordinaryMixedSeed <- pure (3 :: Int)

data OrdinaryMixedToken = OrdinaryMixedToken Int
ordinaryMixedRead (OrdinaryMixedToken value) = value + ordinaryMixedSeed

ordinaryMixedValue <- pure (OrdinaryMixedToken 39)
display (ordinaryMixedRead ordinaryMixedValue == 42)
