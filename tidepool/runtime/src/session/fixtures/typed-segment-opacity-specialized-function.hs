opacityStrict :: Num value => value -> Int
{-# INLINE opacityStrict #-}
{-# SPECIALIZE opacityStrict :: Int -> Int #-}
opacityStrict value = (value + 1) `seq` opacitySeed
