worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "operator-worker"))
let requestKey = [label|operator-request|]
answer <- request @Int worker (assignment requestKey (41 :: Int))
