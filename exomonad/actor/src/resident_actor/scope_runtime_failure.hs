_ <- do
  result <- do
    outcome <- Scope.withScope $ \scope -> do
      _ <- admitScoped scope "scope-failed-child"
      Effects.error "deliberate scope body failure" >> pure ()
    case (Scope.scopeBody outcome, Scope.scopeCleanup outcome) of
      (Left (Scope.ScopeEvaluationFailed detail), Right ())
        | Text.isInfixOf "deliberate scope body failure" detail -> pure True
      (Left failure, cleanup) -> Effects.error ("unexpected scope body failure: " <> tshow (failure, cleanup))
      _ -> Effects.error "body failure and cleanup outcome were conflated"
  say (tshow result)
