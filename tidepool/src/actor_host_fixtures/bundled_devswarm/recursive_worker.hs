data NestedProtocol result
-- TIDEPOOL-ITEM --
nestedDefinition :: ActorDefinition () NestedProtocol ()
nestedDefinition = ActorDefinition
  { label = "nested-review"
  , effectProfile = ReadOnly
  , initialization = pure
  , behavior = \_ _ -> do
      action <-
        ( agentSession (Just "inspect nested boundary") ()
            :: Eff (ReadOnlyEffects NestedProtocol) (AgentAction (ReadOnlyEffects NestedProtocol) ())
        )
      outcome <- runAgentAction action
      case outcome of
        Right () -> pure ()
        Left failure -> error (show failure)
  , onShutdown = const (pure ())
  }
-- TIDEPOOL-ITEM --
do
  _ <- startActor nestedDefinition ()
  complete (pure (WorkerReport { summary = "spawned nested actor", evidence = [] }))
