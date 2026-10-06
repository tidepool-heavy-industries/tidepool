module ScheduleRight where

import ScheduleShared

{-# NOINLINE right #-}
right :: Int -> Int
right value = shared value + 3
