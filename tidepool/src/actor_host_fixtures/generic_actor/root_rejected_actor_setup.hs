data DeadProtocol result where DeadCall :: DeadProtocol Int
-- TIDEPOOL-ITEM --
deadActor :: ActorDefinition () DeadProtocol ()
-- TIDEPOOL-ITEM --
:{
deadActor = ActorDefinition
  { label = "already-finished"
  , effectProfile = ReadOnly
  , initialization = pure
  , behavior = \_ _ -> pure ()
  , onShutdown = const (pure ())
  }
:}
