import Tidepool.Actor hiding (Source)
let counter = (stateful "drain-counter" (Selected (knownEffects @'[])) (\state (delta, reply) -> pure (reply, state + delta)) :: ActorDefinition Int ((,) Int) Int)
server <- startActor counter 7
mapM_ (\delta -> cast server (delta, ())) [1..20]
drainActor server
finished <- awaitExit server
again <- awaitExit server
display (finished == Completed 217 && again == finished)
