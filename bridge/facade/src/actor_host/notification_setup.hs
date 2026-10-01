worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "notification-recipient"))
let requestName = [label|notification-original|]
answer <- request @Text worker (assignment requestName ("original assignment" :: Text))
