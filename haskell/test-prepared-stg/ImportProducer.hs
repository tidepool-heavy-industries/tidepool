module ImportProducer where

-- | A retained-generation symbol from an earlier session turn, compiled as a
-- home module alongside its consumer. Nothing here withholds these bindings
-- from GHC's simplifier -- 'Tidepool.RetainedUnfoldings' does that for any
-- symbol in the pipeline's retained-generation map, so a notebook user never
-- has to write a pragma for it.
producerValue :: [Int]
producerValue = [1, 2, 3]

producerFn :: Int -> Int
producerFn n = n + length producerValue
