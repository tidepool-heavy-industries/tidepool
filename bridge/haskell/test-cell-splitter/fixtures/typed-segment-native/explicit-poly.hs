let identity :: forall a. a -> a; identity x = x
intValue <- pure (identity (12 :: Int))
boolValue <- pure (identity True)
