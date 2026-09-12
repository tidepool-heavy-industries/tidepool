worker <- startAgent (readonlyAgent "notification-recipient")
let requestName = "notification-original" :: RequestLabel
answer <- request @Text worker requestName ("original assignment" :: Text)
