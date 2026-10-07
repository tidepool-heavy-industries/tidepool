{-# LANGUAGE NoMonomorphismRestriction #-}
let step x = x + 1
let alias = step
intValue <- pure (alias (2 :: Int))
doubleValue <- pure (if alias (2 :: Double) == 3 then (4 :: Int) else 0)
