module ScheduleLeft where

import ScheduleShared

{-# NOINLINE left #-}
left :: Int -> Int
left value = shared value * 2
