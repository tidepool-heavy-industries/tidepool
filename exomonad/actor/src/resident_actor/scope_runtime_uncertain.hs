_ <- do
  result <- do
    outcome <- Scope.withScope $ \scope -> do
      started <- Cmd.tryStartWith (Core.InScope scope) (Cmd.argv ["scope-cleanup-uncertain"])
      case started of
        Left _ -> error "uncertain command admission refused"
        Right job -> waitCommandRunning job >> pure (42 :: Int)
    case (Scope.scopeBody outcome, Scope.scopeCleanup outcome) of
      (Right 42, Left (Scope.ScopeCleanupUnconfirmed _)) -> pure True
      _ -> error "cleanup uncertainty replaced the successful body value"
  say (tshow result)
