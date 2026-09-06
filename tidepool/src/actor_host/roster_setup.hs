first <- startAgent (readonlyAgent "roster-first")
second <- startAgent (readonlyAgent "roster-second")
let Right firstLabel = requestLabel "roster-first-request"
let Right secondLabel = requestLabel "roster-second-request"
firstAnswer <- request @Int first firstLabel (10 :: Int)
secondAnswer <- request @Int second secondLabel (20 :: Int)
