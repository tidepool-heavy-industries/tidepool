data NestedProtocol result
-- TIDEPOOL-ITEM --
nestedDefinition :: ActorDefinition () NestedProtocol ()
nestedDefinition = ActorDefinition
  { label = "nested-review"
  , effectProfile = ReadOnly
  , initialization = pure
  , behavior = \_ _ -> agentSession (Just "inspect nested boundary") ()
  , visibleToChild = []
  , onShutdown = const (pure ())
  }
-- TIDEPOOL-ITEM --
do
  _ <- startActor nestedDefinition ()
  complete (WorkerReport { summary = "spawned nested actor", evidence = [] })
