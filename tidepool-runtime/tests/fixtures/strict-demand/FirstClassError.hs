module FirstClassError where

import Prelude

{-# NOINLINE select #-}
select :: Bool -> String -> Int
select b = if b then error else length

{-# NOINLINE demand #-}
demand :: (String -> Int) -> Int
demand f = f `seq` 42

result :: Int
result = demand (select True)
