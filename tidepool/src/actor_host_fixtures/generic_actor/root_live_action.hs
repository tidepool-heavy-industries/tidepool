data FunctionProtocol result
-- TIDEPOOL-ITEM --
functionActor :: Text -> (Int -> Int) -> ActorDefinition () FunctionProtocol (Int -> Int)
-- TIDEPOOL-ITEM --
:{
functionActor actorLabel fn = ActorDefinition
  { label = actorLabel
  , effectProfile = ReadOnly
  , initialization = pure
  , behavior = \_ _ -> pure fn
  , onShutdown = const (pure ())
  }
:}
-- TIDEPOOL-ITEM --
data ValueProtocol result
-- TIDEPOOL-ITEM --
valueActor :: Text -> Int -> ActorDefinition () ValueProtocol Int
-- TIDEPOOL-ITEM --
:{
valueActor actorLabel value = ActorDefinition
  { label = actorLabel
  , effectProfile = ReadOnly
  , initialization = pure
  , behavior = \_ _ -> pure value
  , onShutdown = const (pure ())
  }
:}
-- TIDEPOOL-ITEM --
:{
do
  transform <- startActor (functionActor "function-worker" ((+ 1) :: Int -> Int)) ()
  input <- startActor (valueActor "value-worker" 20) ()
  complete $ nextTurn $ do
    assembled <- ($) <$> waitOn transform <*> waitOn input
    dependent <- liftAction $ startActor (valueActor "dependent-worker" (assembled * 2)) ()
    waitOn dependent
:}
