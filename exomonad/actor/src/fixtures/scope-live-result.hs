do
  outcome <- Scope.withScope (\_ -> pure ((+) (41 :: Int)))
  pure (case (Scope.scopeBody outcome, Scope.scopeCleanup outcome) of
    (Right value, Right ()) -> value 1 == 42
    _ -> False)
