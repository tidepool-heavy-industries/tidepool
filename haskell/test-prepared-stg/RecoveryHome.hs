module RecoveryHome where

homeValue :: Int -> Int
homeValue value = value
{-# NOINLINE homeValue #-}

homeOther :: Int -> Int
homeOther value = homeValue value
{-# NOINLINE homeOther #-}
