{-# LANGUAGE NoMonomorphismRestriction #-}
let number = 7
intValue <- pure (number + 2 :: Int)
doubleValue <- pure (if (number + 3 :: Double) == 10 then (11 :: Int) else 0)
