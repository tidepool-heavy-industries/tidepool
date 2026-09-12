first <- startAgent (readonlyAgent "roster-first")
second <- startAgent (readonlyAgent "roster-second")
let firstLabel = "roster-first-request" :: RequestLabel
let secondLabel = "roster-second-request" :: RequestLabel
firstAnswer <- request @Int first firstLabel (10 :: Int)
secondAnswer <- request @Int second secondLabel (20 :: Int)
