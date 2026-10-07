{-# LANGUAGE Strict #-}
number <- pure (7 :: Int)
let identity value = value
answer <- pure (identity number + 1)
