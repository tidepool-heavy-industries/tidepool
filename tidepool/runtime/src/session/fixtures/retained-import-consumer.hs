consumerValue :: [Int]
consumerValue = producerValue

consumerResult :: Int
consumerResult = producerFn (length producerValue)
