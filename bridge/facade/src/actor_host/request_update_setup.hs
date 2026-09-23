worker <- startAgent (readonlyAgent "tabs-worker")
let tabsLabel = [label|tabs|]
answer <- request @Int worker (assignment tabsLabel (10 :: Int))
