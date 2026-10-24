module NativeBoundarySource (seed, sourceId, sourceRule) where

seed :: Int
seed = hideSeed 23

-- The input constructor must be unknown, as it is for a captured value's
-- thin interface. This fixture must not supply a statically reducible Int.
hideSeed :: value -> value
hideSeed value = value
{-# OPAQUE hideSeed #-}

sourceId :: Int -> Int
sourceId value = value
{-# OPAQUE sourceId #-}

sourceRule :: Int -> Int
sourceRule value = value
{-# OPAQUE sourceRule #-}
{-# RULES "source-library-rule" forall value. sourceRule value = value + 1 #-}
