do
  if sessionInput 20 == (42 :: Int)
    then complete (pure ())
    else error "composed live actor closure returned the wrong result"
