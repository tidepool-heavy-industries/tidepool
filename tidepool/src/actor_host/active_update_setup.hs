worker <- startAgent (readonlyAgent "tabs-worker")
let tabsLabel = "tabs" :: RequestLabel
answer <- request @Int worker tabsLabel (10 :: Int)
