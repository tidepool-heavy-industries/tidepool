_ <- do
  result <- do
    outcome <- Scope.withScope $ \scope -> do
      owned <- admitScoped scope "scope-owned-child"
      spawned <- spawnScoped scope "scope-retained-child"
      survivor <- case spawned of
        Left _ -> error "retained child admission refused"
        Right agent -> pure agent
      retained <- Agents.retainAgent survivor Core.ActorOwned
      case retained of
        Left _ -> error "actor retention refused"
        Right () -> pure (scope, survivor, owned)
    case (Scope.scopeBody outcome, Scope.scopeCleanup outcome) of
      (Right (escaped, _, (owned, pending, job)), Right ()) -> do
        childState <- Agents.observeAgent owned
        requestState <- Reply.pollResponse pending
        commandState <- Cmd.status job
        case (Agents.observedState childState, requestState, commandState) of
          (Agents.AgentCancelled _, Reply.ResponseUnavailable Reply.ResponseCancelled, Cmd.CommandFinished result)
            | Cmd.commandCleanup result == Cmd.CommandClean -> pure ()
          _ -> error "resources outlived their lexical scope"
        child <- spawnScoped escaped "scope-closed-child"
        command <- Cmd.tryStartWith (Core.InScope escaped) (Cmd.argv ["scope-closed-command"])
        request <- Agents.request @Int scopeTarget ()
          (Agents.defaultRequestOptions { Agents.requestLifetime = Core.InScope escaped })
        case (child, command, request) of
          (Left _, Left _, Left _) -> pure True
          _ -> error "closed scope admitted new work"
      _ -> error "normal scope body or cleanup failed"
  say (tshow result)
