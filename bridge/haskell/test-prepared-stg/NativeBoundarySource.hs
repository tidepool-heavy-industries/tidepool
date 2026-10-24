module NativeBoundarySource (seed, sourceId, sourceRule) where

seed :: Int
seed = 23

sourceId :: Int -> Int
sourceId value = value
{-# NOINLINE sourceId #-}

sourceRule :: Int -> Int
sourceRule value = value
{-# NOINLINE sourceRule #-}
{-# RULES "source-library-rule" forall value. sourceRule value = value + 1 #-}
