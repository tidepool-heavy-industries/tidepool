data FunctionProtocol result
-- TIDEPOOL-ITEM --
functionActor :: Text -> (Int -> Int) -> ActorDefinition () FunctionProtocol (Int -> Int)
-- TIDEPOOL-ITEM --
functionActor actorLabel fn = ActorDefinition
      { label = actorLabel
      , effectProfile = ReadOnly
      , initialization = pure
      , behavior = \_ _ -> pure fn
      , onShutdown = const (pure ())
      }
-- TIDEPOOL-ITEM --
increment <- startActor (functionActor "increment" ((+ 1) :: Int -> Int)) ()
-- TIDEPOOL-ITEM --
double <- startActor (functionActor "double" ((* 2) :: Int -> Int)) ()
-- TIDEPOOL-ITEM --
complete $ nextTurn $ (.) <$> waitOn double <*> waitOn increment
