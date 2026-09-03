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
:{
do
  failedChild <- startActor failingActor ()
  _ <- cast failedChild FailNow
  complete $ nextTurn $ waitOn failedChild
:}
