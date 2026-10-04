producerValue <- pure ([1, 2, 3] :: [Int])
producerFn <- pure (\n -> n + length producerValue)
