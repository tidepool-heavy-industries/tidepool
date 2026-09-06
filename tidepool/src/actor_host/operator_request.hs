worker <- startAgent (readonlyAgent "operator-worker")
let Right requestKey = requestLabel "operator-request"
answer <- request @Int worker requestKey (41 :: Int)
