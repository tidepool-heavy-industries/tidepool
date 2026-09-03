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
:{
do
  increment <- startActor (functionActor "function-worker" ((+ 1) :: Int -> Int)) ()
  double <- startActor (functionActor "function-worker" ((* 2) :: Int -> Int)) ()
  complete $ nextTurn $ (.) <$> waitOn double <*> waitOn increment
:}
