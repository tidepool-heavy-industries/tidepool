worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "operator-worker"))
let requestKey = [label|operator-request|]
answer <- do { issued <- request @Int worker (assignment requestKey (41 :: Int)); Right () <- detachRequest issued; pure issued }
