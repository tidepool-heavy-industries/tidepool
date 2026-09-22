module ImportProducerExposed where

-- | Same two bindings as 'ImportProducer', but with NO 'NOINLINE' pragmas:
-- this is the shape a notebook user actually writes. Withholding these two
-- bindings' unfoldings from GHC's simplifier is
-- 'Tidepool.RetainedUnfoldings''s job when they are retained-generation
-- symbols; 'ExecutionProjectionTest.verifyRetainedImportProjectionExposed'
-- proves that pass is what makes this module safe to compile plainly.
producerValue :: [Int]
producerValue = [1, 2, 3]

producerFn :: Int -> Int
producerFn n = n + length producerValue
