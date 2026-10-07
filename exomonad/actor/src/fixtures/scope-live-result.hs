do
  outcome <- Scope.withScope (\_ -> pure ((+) (41 :: Int)))
  case (Scope.scopeBody outcome, Scope.scopeCleanup outcome) of
    (Right value, Right ()) ->
      if value 1 == 42
        then pure ()
        else error "scope returned a closure with the wrong result"
    _ -> error "scope body or cleanup failed"
