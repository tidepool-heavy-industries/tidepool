worker <- startAgent (readonlyAgent "operator-worker")
let requestKey = "operator-request" :: Label
answer <- request @Int worker requestKey (41 :: Int)
