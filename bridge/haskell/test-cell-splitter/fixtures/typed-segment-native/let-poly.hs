let identity x = x
let constant x _ = x
intValue <- pure (identity (3 :: Int))
boolValue <- pure (identity True)
constantValue <- pure (constant (7 :: Int) False)
