_ <- do
  result <- do
    outcome <- Scope.withScope $ \scope -> do
      _ <- admitScoped scope "scope-failed-child"
      Effects.error "deliberate scope body failure" >> pure ()
    case (Scope.scopeBody outcome, Scope.scopeCleanup outcome) of
      (Left (Scope.ScopeEvaluationFailed _), Right ()) -> pure True
      _ -> Effects.error "body failure and cleanup outcome were conflated"
  say (tshow result)
