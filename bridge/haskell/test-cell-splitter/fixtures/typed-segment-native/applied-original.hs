function <- pure (id (\value -> undefined `asTypeOf` value))
answer <- pure (function (2 :: Int))
