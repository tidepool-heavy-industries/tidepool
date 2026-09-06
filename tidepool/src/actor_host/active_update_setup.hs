worker <- startAgent (readonlyAgent "tabs-worker")
let Right tabsLabel = requestLabel "tabs"
answer <- request @Int worker tabsLabel (10 :: Int)
