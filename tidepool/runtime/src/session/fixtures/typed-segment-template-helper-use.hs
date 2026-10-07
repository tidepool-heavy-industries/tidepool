replyValue <- respond (4 :: Int)
recursiveValue <- recursiveEven 3
integerStep <- pure (helperStep (2 :: Int))
doubleStep <- pure (helperStep (2 :: Double))
inferredInteger <- pure (inferredStep (4 :: Int))
inferredDouble <- pure (inferredStep (4 :: Double))
reportHelpers replyValue recursiveValue integerStep doubleStep inferredInteger inferredDouble
