replyValue <- respond (4 :: Int)
recursiveValue <- recursiveEven 3
let integerStep = helperStep (2 :: Int)
let doubleStep = helperStep (2 :: Double)
let inferredInteger = inferredStep (4 :: Int)
let inferredDouble = inferredStep (4 :: Double)
reportHelpers replyValue recursiveValue integerStep doubleStep inferredInteger inferredDouble
