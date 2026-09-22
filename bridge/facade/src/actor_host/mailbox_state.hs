import Tidepool.Actor
let definition = (ActorDefinition
      { label = "stateful-mailbox"
      , effectProfile = ReadOnly
      , initialization = pure
      , behavior = \_ initial -> do
          next <- receive (\(delta, answer) -> pure (answer, initial + delta))
          receive (\(delta, answer) -> pure (answer, next + delta))
      , onShutdown = const (pure ())
      } :: ActorDefinition Int ((,) Int) Int)
server <- startActor definition 7
first <- call server (3, ((+ 1) :: Int -> Int))
second <- call server (5, True)
finished <- awaitExit server
first 41 == 42 && second && finished == Completed 15
