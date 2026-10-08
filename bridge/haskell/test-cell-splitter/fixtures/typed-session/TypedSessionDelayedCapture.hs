let first = 11 :: Int
_ <- (pure () :: Eff '[] ())
_ <- (pure () :: Eff '[] ())
let delayed = first + 7
_ <- (pure () :: Eff '[] ())
let later = delayed + first
