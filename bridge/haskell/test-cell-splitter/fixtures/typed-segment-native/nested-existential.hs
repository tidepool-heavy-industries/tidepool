name <- pure (case Some (41 :: Int) of Some (_ :: a) -> show (typeRep (Proxy @a)))
