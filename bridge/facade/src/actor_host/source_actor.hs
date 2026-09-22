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
      ] (stateful "ordered-source-collector" ReadOnly (\values (value, reply) -> pure (reply, value : values)) :: ActorDefinition [Int] ((,) Int) [Int])
collector <- startActor collectorDefinition []
