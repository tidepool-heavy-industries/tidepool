packed <- pure (Some (41 :: Int))
Some (_ :: a) <- pure packed
let name = show (typeRep (Proxy @a))
