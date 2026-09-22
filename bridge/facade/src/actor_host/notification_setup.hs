worker <- startAgent (readonlyAgent "notification-recipient")
let requestName = "notification-original" :: Label
answer <- request @Text worker (assignment requestName ("original assignment" :: Text))
