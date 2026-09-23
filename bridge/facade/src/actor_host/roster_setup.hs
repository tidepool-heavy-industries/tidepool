first <- startAgent (readonlyAgent "roster-first")
second <- startAgent (readonlyAgent "roster-second")
let firstLabel = [label|roster-first-request|]
let secondLabel = [label|roster-second-request|]
firstAnswer <- request @Int first (assignment firstLabel (10 :: Int))
secondAnswer <- request @Int second (assignment secondLabel (20 :: Int))
