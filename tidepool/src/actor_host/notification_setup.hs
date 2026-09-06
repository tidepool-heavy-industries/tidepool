worker <- startAgent (readonlyAgent "notification-recipient")
let Right requestName = requestLabel "notification-original"
answer <- request @Text worker requestName ("original assignment" :: Text)
