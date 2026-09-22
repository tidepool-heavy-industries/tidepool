worker <- startAgent (readonlyAgent "operator-worker")
let requestKey = "operator-request" :: Label
answer <- request @Int worker (assignment requestKey (41 :: Int))
