worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "notification-recipient"))
let requestName = [label|notification-original|]
answer <- do { issued <- request @Text worker (assignment requestName ("original assignment" :: Text)); Right () <- detachRequest issued; pure issued }
