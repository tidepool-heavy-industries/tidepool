do
      answer <- send (ReflectWith 2)
      case answer of
        Left ReflectUnbound -> pure (37 :: Int)
        _ -> error "a context with no conversation must not be given one"
