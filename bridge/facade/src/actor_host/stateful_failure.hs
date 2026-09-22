import Tidepool.Actor
data Counter result = Counter Int (Int -> result)
let counter = (stateful "effect-counter" ReadOnly (\state (Counter delta answer) -> pure (answer state, state + delta)) :: ActorDefinition Int Counter Int)
sink <- startActor counter 0
let definition = (stateful "retained-state" ReadOnly (\state (Counter delta answer) -> if delta < 0 then do { cast sink (Counter 1 (const ())); error "handler-probe" } else pure (answer state, state + delta)) :: ActorDefinition Int Counter Int)
server <- startActor definition 7
first <- call server (Counter 3 (\state -> (+ state)))
second <- call server (Counter 5 id)
first 35 == 42 && second == 10
