worker <- startAgent (readonlyAgent "tabs-worker")
let tabsLabel = "tabs" :: Label
answer <- request @Int worker tabsLabel (10 :: Int)
