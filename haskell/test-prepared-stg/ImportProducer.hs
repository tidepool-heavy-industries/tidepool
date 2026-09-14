module ImportProducer where

-- NOINLINE models what a retained binding from an earlier session turn is
-- to a later program: a symbol whose body is not available for inlining.
-- Without it GHC inlines producerValue's static list (as floated
-- producerValue1..5 sub-bindings) into a consumer compiled alongside this
-- module, and the consumer's artifact then carries a recovered copy instead
-- of a Global reference -- retained-generation matching is external-name-only,
-- so the floated internal names never match the retained map.
producerValue :: [Int]
producerValue = [1, 2, 3]
{-# NOINLINE producerValue #-}

producerFn :: Int -> Int
producerFn n = n + length producerValue
{-# NOINLINE producerFn #-}
