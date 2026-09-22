module ImportProducer where

-- OPAQUE is the projection-only stand-in for an earlier session binding
-- whose body is unavailable to a later program. ImportProducerExposed covers
-- the production compiler path where Tidepool.RetainedUnfoldings withholds
-- the same definitions without requiring authored pragmas.
producerValue :: [Int]
producerValue = [1, 2, 3]
{-# OPAQUE producerValue #-}

producerFn :: Int -> Int
producerFn n = n + length producerValue
{-# OPAQUE producerFn #-}
