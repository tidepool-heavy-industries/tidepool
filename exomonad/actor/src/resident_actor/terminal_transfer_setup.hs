let definition :: Mailbox.ActorDefinition () AgentProtocol ()
    definition = Mailbox.ActorDefinition
      { Mailbox.label = "terminal-transfer-target"
      , Mailbox.effectProfile = Mailbox.ReadOnly
      , Mailbox.initialization = \() -> pure ()
      , Mailbox.behavior = \() () ->
          Mailbox.serve @() @AgentProtocol ()
            (\() (RunRequest action) -> action >> pure ((), ()))
      , Mailbox.onShutdown = const (pure ())
      }
nativeAgent <- do
  actor <- Mailbox.startActor definition ()
  pure (AgentRef.AgentRef actor Nothing)
pure True
