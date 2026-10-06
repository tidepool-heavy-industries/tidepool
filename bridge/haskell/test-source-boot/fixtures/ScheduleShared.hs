module ScheduleShared where

{-# NOINLINE shared #-}
shared :: Int -> Int
shared value = value + 1
