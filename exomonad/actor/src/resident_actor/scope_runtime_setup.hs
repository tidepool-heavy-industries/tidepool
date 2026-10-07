let scopeSpec :: AgentSpec ScopeTools '[]
    scopeSpec = defaultSpec
      { specTools = ScopeTools
          { ping = tool "Read a retained scope child closure." $ \_ -> pure (73 :: Int)
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
let spawnScoped scope label = Spawn.spawnSubagent
      (Spawn.FreshCtx "Idle resource scope fixture") Spawn.SameDir
      ((Spawn.defaultSpawnOptions scopeSpec)
        { Spawn.spawnLifetime = Core.InScope scope, Spawn.spawnLabel = Just label })
let waitCommandRunning job = do
      status <- Cmd.status job
      case status of
        Cmd.CommandRunning -> pure ()
        Cmd.CommandStarting -> sleep (milliseconds 1) >> waitCommandRunning job
        Cmd.CommandQueued -> sleep (milliseconds 1) >> waitCommandRunning job
        _ -> error "command did not reach its controlled execution"
let admitScoped scope label = do
      admitted <- spawnScoped scope label
      child <- case admitted of
        Left _ -> error "scope child admission refused"
        Right agent -> pure agent
      requested <- Agents.request @Int scopeTarget ()
        (Agents.defaultRequestOptions { Agents.requestLifetime = Core.InScope scope })
      request <- case requested of
        Left _ -> error "scope request admission refused"
        Right pending -> pure pending
      started <- Cmd.tryStartWith (Core.InScope scope) (Cmd.argv [label])
      job <- case started of
        Left _ -> error "scope command admission refused"
        Right job -> pure job
      waitCommandRunning job
      pure (child, request, job)
pure True
