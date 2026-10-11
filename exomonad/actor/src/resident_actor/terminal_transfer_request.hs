response <- do
  admitted <- request @Int nativeAgent () defaultRequestOptions
  pending <- case admitted of
    Left _ -> error "original request admission refused"
    Right request -> pure request
  retained <- retainRequest pending ActorOwned
  case retained of
    Left _ -> error "original request retention refused"
    Right () -> pure pending
pure True
