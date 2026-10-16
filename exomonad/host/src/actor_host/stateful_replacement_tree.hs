import Tidepool.Actor hiding (Source)
data Counter result = Counter Int (Int -> result)
let counter = (stateful "owned-counter" (Selected (knownEffects @'[])) (\state (Counter delta answer) -> pure (answer state, state + delta)) :: ActorDefinition Int Counter Int)
data Ownership result = SpawnOwned (ActorRef Counter Int -> result)
let owningDefinition = (stateful "replacement-owner" (Selected (knownEffects @'[Actor, AgentLaunch])) (\state (SpawnOwned answer) -> do { owned <- startActor counter state; pure (answer owned, state) }) :: ActorDefinition Int Ownership Int)
owner <- startActor owningDefinition 31
owned <- call owner (SpawnOwned id)
owner2 <- replaceActor owner owningDefinition
childState <- call owned (Counter 0 id)
let ownershipStateCorrect = childState == 31
drainActor owner2
ownerExit <- awaitExit owner2
childExit <- pollExit owned
display (ownershipStateCorrect && case (ownerExit, childExit) of { (Completed 31, Just (Cancelled _)) -> True; _ -> False })
