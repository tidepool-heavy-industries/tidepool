first <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "roster-first"))
second <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "roster-second"))
let firstLabel = [label|roster-first-request|]
let secondLabel = [label|roster-second-request|]
firstAnswer <- do { issued <- request @Int first (assignment firstLabel (10 :: Int)); Right () <- detachRequest issued; pure issued }
secondAnswer <- do { issued <- request @Int second (assignment secondLabel (20 :: Int)); Right () <- detachRequest issued; pure issued }
