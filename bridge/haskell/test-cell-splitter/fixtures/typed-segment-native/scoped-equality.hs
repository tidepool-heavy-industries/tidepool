let project :: forall a. Equal a -> a -> Int; project EInt value = value
answer <- pure (project EInt (12 :: Int))
