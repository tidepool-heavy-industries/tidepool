module ImportProducer where

producerValue :: [Int]
producerValue = [1, 2, 3]

producerFn :: Int -> Int
producerFn n = n + length producerValue
