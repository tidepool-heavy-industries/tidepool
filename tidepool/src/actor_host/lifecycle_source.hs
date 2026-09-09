import Tidepool.Actor
data Counter result = Counter Int (Int -> result)
let counter = (stateful "lifecycle-counter" ReadOnly (\state (Counter delta answer) -> pure (answer state, state + delta)) :: ActorDefinition Int Counter Int)
worker <- startActor counter 7
data LifecycleMessage result = RecordLifecycle ActorLifecycle result | ReadLifecycle ([ActorLifecycle] -> result)
let collect state message = case message of
      RecordLifecycle event reply -> (reply, event : state)
      ReadLifecycle answer -> (answer (reverse state), state)
let collectorDefinition = withSources [lifecycleSource worker (\event -> RecordLifecycle event ())] (stateful "lifecycle-collector" ReadOnly (\state message -> pure (collect state message)) :: ActorDefinition [ActorLifecycle] LifecycleMessage [ActorLifecycle])
-- STAGE --
collector <- startActor collectorDefinition []
-- STAGE --
first <- call collector (ReadLifecycle id)
first == [ActorLive]
-- STAGE --
collector2 <- replaceActor collector collectorDefinition
-- STAGE --
drainActor worker
workerExit <- awaitExit worker
events <- call collector2 (ReadLifecycle id)
case events of { [ActorLive, ActorFinished _] -> True; _ -> False }
-- STAGE --
late <- startActor collectorDefinition []
lateEvents <- call late (ReadLifecycle id)
case lateEvents of { [ActorFinished _] -> True; _ -> False }
-- STAGE --
drainActor collector2
drainActor late
