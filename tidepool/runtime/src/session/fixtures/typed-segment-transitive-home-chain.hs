chainCurrentValue <- segmentRecord (11 :: Int) >> pure (23 :: Int)
{-# NOINLINE chainStep #-}
chainStep :: Int -> Int
chainStep value = chainHistorical value + chainCurrentValue
chainBarrier <- pure (0 :: Int)
{-# NOINLINE chainTail #-}
chainTail :: Int -> Int
chainTail value = chainStep value + chainBarrier
segmentRecord (chainTail 2)
