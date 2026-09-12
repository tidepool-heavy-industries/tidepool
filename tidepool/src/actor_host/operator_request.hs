worker <- startAgent (readonlyAgent "operator-worker")
let requestKey = "operator-request" :: RequestLabel
answer <- request @Int worker requestKey (41 :: Int)
