module ModuleProductA (Box(..), produce) where

data Box = Box Int

{-# NOINLINE seed #-}
seed :: Int
seed = 40

{-# NOINLINE evenStep #-}
evenStep :: Int -> Bool
evenStep n = n == 0 || oddStep (n - 1)

{-# NOINLINE oddStep #-}
oddStep :: Int -> Bool
oddStep n = n /= 0 && evenStep (n - 1)

{-# NOINLINE produce #-}
produce :: Int
produce = if evenStep seed then seed + 1 else 0
