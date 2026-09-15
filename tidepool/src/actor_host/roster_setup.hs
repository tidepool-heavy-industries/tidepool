first <- startAgent (readonlyAgent "roster-first")
second <- startAgent (readonlyAgent "roster-second")
let firstLabel = "roster-first-request" :: Label
let secondLabel = "roster-second-request" :: Label
firstAnswer <- request @Int first (assignment firstLabel (10 :: Int))
secondAnswer <- request @Int second (assignment secondLabel (20 :: Int))
