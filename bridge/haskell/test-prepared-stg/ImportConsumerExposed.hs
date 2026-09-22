module ImportConsumerExposed where

import ImportProducerExposed (producerFn, producerValue)

-- | Mirrors 'ImportConsumer.consumerResult': the entry
-- 'ExecutionProjectionTest.verifyRetainedImportProjectionExposed' projects.
-- Without 'Tidepool.RetainedUnfoldings' withholding 'producerValue' and
-- 'producerFn''s unfoldings during compilation, GHC inlines both into this
-- binding (floating 'producerValue''s list as top-level
-- @producerValue1@..@producerValue5@ sub-bindings and specialising
-- 'producerFn' into a worker), so the projected program would carry a
-- recovered copy of each instead of a 'GlobalDecl' reference.
consumerResult :: Int
consumerResult = producerFn (length producerValue)
