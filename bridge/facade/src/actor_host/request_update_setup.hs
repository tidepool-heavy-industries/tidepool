worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "tabs-worker"))
let tabsLabel = [label|tabs|]
answer <- do { issued <- request @Int worker (assignment tabsLabel (10 :: Int)); Right () <- detachRequest issued; pure issued }
