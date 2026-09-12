first <- startAgent (readonlyAgent "roster-first")
second <- startAgent (readonlyAgent "roster-second")
let firstLabel = "roster-first-request" :: Label
let secondLabel = "roster-second-request" :: Label
firstAnswer <- request @Int first firstLabel (10 :: Int)
secondAnswer <- request @Int second secondLabel (20 :: Int)
