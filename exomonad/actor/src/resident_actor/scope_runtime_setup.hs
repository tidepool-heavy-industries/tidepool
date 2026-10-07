let scopeSpec :: AgentSpec ScopeTools '[]
    scopeSpec = defaultSpec
      { specTools = ScopeTools
          { ping = presentWith presentDisplay $
              tool "Read a retained scope child closure." $ \_ -> pure (73 :: Int)
          }
      }
let definition :: Mailbox.ActorDefinition () AgentProtocol ()
    definition = Mailbox.ActorDefinition
      { Mailbox.label = "scope-request-target"
      , Mailbox.effectProfile = Mailbox.ReadOnly
      , Mailbox.initialization = \() -> pure ()
      , Mailbox.behavior = \() () ->
          Mailbox.serve @() @AgentProtocol ()
            (\() (RunRequest _) -> pure ((), ()))
      , Mailbox.onShutdown = const (pure ())
      }
scopeTarget <- do
  actor <- Mailbox.startActor definition ()
  pure (AgentRef.AgentRef actor Nothing)
let spawnScoped
      :: forall effects. Member Core.AgentLaunch effects
      => Scope.Scope -> Text -> Eff effects (Either Spawn.SpawnError AgentRef.AgentRef)
    spawnScoped scope label = Spawn.spawnSubagent
      (Spawn.FreshCtx "Idle resource scope fixture") Spawn.SameDir
      ((Spawn.defaultSpawnOptions scopeSpec)
        { Spawn.spawnLifetime = Core.InScope scope, Spawn.spawnLabel = Just label })
let waitCommandRunning
      :: forall effects. (Member Core.Commands effects, Member Core.Sleep effects)
      => Cmd.Job -> Eff effects ()
    waitCommandRunning job = do
      status <- Cmd.status job
      case status of
        Cmd.CommandRunning -> pure ()
        Cmd.CommandStarting -> sleep (milliseconds 1) >> waitCommandRunning job
        Cmd.CommandQueued -> sleep (milliseconds 1) >> waitCommandRunning job
        _ -> Effects.error "command did not reach its controlled execution"
let admitScoped
      :: forall effects.
         ( Member Core.AgentLaunch effects, Member Replies effects
         , Member Core.Commands effects, Member Core.Sleep effects )
      => Scope.Scope -> Text -> Eff effects (AgentRef.AgentRef, Reply.Request Int, Cmd.Job)
    admitScoped scope label = do
      admitted <- spawnScoped scope label
      child <- case admitted of
        Left _ -> Effects.error "scope child admission refused"
        Right agent -> pure agent
      requested <- Agents.request @Int scopeTarget ()
        (Agents.defaultRequestOptions { Agents.requestLifetime = Core.InScope scope })
      request <- case requested of
        Left _ -> Effects.error "scope request admission refused"
        Right pending -> pure pending
      started <- Cmd.tryStartWith (Core.InScope scope) (Cmd.argv [label])
      job <- case started of
        Left _ -> Effects.error "scope command admission refused"
        Right job -> pure job
      waitCommandRunning job
      pure (child, request, job)
_ <- say (tshow True)
