{-# LANGUAGE NoMonomorphismRestriction #-}
number <- pure (read "7")
answer <- pure (number + (1 :: Int))
