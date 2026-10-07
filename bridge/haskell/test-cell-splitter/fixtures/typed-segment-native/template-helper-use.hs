replyValue <- respond (4 :: Int)
recursiveValue <- recursiveEven 3
let integerStep = helperStep (2 :: Int)
let doubleStep = helperStep (2 :: Double)
pure (replyValue, recursiveValue, integerStep, doubleStep)
