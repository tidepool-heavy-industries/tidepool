data FailingProtocol result where FailNow :: FailingProtocol ()
-- TIDEPOOL-ITEM --
failStep :: FailingProtocol result -> Eff (ReadOnlyEffects FailingProtocol) (result, ())
-- TIDEPOOL-ITEM --
failStep FailNow = error "intentional child failure"
-- TIDEPOOL-ITEM --
failingActor :: ActorDefinition () FailingProtocol ()
-- TIDEPOOL-ITEM --
:{
failingActor = ActorDefinition
  { label = "failing-child"
  , effectProfile = ReadOnly
  , initialization = pure
  , behavior = \_ _ -> receive @() @FailingProtocol failStep
  , onShutdown = const (pure ())
  }
:}
-- TIDEPOOL-ITEM --
data CompletedProtocol result
-- TIDEPOOL-ITEM --
completedActor :: ActorDefinition () CompletedProtocol ()
-- TIDEPOOL-ITEM --
:{
completedActor = ActorDefinition
  { label = "completed-child"
  , effectProfile = ReadOnly
  , initialization = pure
  , behavior = \_ _ -> pure ()
  , onShutdown = const (pure ())
  }
:}
-- TIDEPOOL-ITEM --
:{
do
  completedChild <- startActor completedActor ()
  failedChild <- startActor failingActor ()
  _ <- cast failedChild FailNow
  complete $ nextTurn $ do
    liftAction $ do
      outcome <- awaitExit completedChild
      case outcome of
        Completed () -> pure ()
        Failed _ -> error "completed child failed"
        Cancelled _ -> error "completed child was cancelled"
    _ <- waitOn failedChild
    liftAction $ error "waitOn failure did not short-circuit the action"
:}
