let repaired = (stateful "repaired-counter" ReadOnly (\state (Counter delta answer) -> pure (answer state, state + delta)) :: ActorDefinition Int Counter Int)
server2 <- replaceActor server repaired
current <- call server2 (Counter 0 id)
effectsAfter <- call sink (Counter 0 id)
current == 20 && effectsAfter == 1
drainActor server2
finalState <- awaitExit server2
case finalState of { Completed value -> value == 20; _ -> False }
oldExit <- pollExit server
case oldExit of { Just (Cancelled _) -> True; _ -> False }
