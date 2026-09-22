worker <- startAgent (readonlyAgent "tabs-worker")
let tabsLabel = "tabs" :: Label
answer <- request @Int worker (assignment tabsLabel (10 :: Int))
