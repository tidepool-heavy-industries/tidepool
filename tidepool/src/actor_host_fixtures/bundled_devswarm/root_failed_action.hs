data FailingProtocol result where FailNow :: FailingProtocol ()
-- TIDEPOOL-ITEM --
failingActor :: ActorDefinition () FailingProtocol ()
failingActor = ActorDefinition
  { label = "failing-child"
  , effectProfile = ReadOnly
  , initialization = pure
  , behavior = \_ _ -> receive (\FailNow -> error "intentional child failure")
  , onShutdown = const (pure ())
  }
-- TIDEPOOL-ITEM --
failedChild <- startActor failingActor ()
-- TIDEPOOL-ITEM --
_ <- cast failedChild FailNow
-- TIDEPOOL-ITEM --
complete $ nextTurn $ waitOn failedChild
