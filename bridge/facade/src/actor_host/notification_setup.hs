worker <- startAgent (readonlyAgent "notification-recipient")
let requestName = [label|notification-original|]
answer <- request @Text worker (assignment requestName ("original assignment" :: Text))
