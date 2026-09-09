import Tidepool.Actor
let collectProgress event = case event of
      ProgressUpdate _ (ProgressNote _ f) -> (f 3, ())
      ProgressClosed -> (-1, ())
      _ -> error "unexpected progress source event"
let collectorDefinition = withSources
      [ progressSource updates collectProgress
      , settlementSource answer (\result -> case result of
          Right receipt -> (responseValue receipt, ())
          Left _ -> error "unexpected failed request")
      ] (ActorDefinition
      { label = "ordered-source-collector"
      , effectProfile = ReadOnly
      , initialization = pure
      , behavior = \_ _ -> sequence
          [ receive (\(value, reply) -> pure (reply, value))
          | _ <- [1 .. 5 :: Int]
          ]
      , onShutdown = const (pure ())
      } :: ActorDefinition () ((,) Int) [Int])
collector <- startActor collectorDefinition ()
