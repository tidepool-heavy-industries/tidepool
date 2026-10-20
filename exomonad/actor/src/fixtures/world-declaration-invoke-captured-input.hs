observed <- case capturedGreeting () of
  CapturedGreeting original ->
    if original == object [("greeting", String "hi")]
       && input == object [("greeting", String "different")]
    then pure (RetainedMaybe.fromMaybe 0 (Just (41 :: Int)))
    else error "published declaration used the current input instead of its captured input"
