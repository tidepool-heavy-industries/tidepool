module ImportConsumer where

import ImportProducer (producerFn, producerValue)

consumerResult :: Int
consumerResult = producerFn (length producerValue)
