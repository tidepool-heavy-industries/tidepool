afterFailure <- case capturedGreeting () of
  CapturedGreeting original ->
    if original == object [("greeting", String "hi")]
       && observed == (41 :: Int)
       && RetainedMaybe.isJust (Just original)
    then pure (42 :: Int)
    else error "failed later cell changed a published declaration, binding or import"
