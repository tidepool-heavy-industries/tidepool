{-# LANGUAGE MagicHash #-}
{-# LANGUAGE UnboxedTuples #-}

module RaiseContract where

import GHC.Exts (Double#, Int#, raise#, raiseDivZero#, raiseUnderflow#)
import GHC.Types (Double(D#), Int(I#))

raisePrimitive :: Int
raisePrimitive = raise# ()

raiseDivZeroPrimitive :: Int
raiseDivZeroPrimitive = raiseDivZero# (# #)

raiseUnderflowPrimitive :: Int
raiseUnderflowPrimitive = raiseUnderflow# (# #)

bottomingUnary :: Int -> Int
bottomingUnary value = raise# value
{-# NOINLINE bottomingUnary #-}

bottomingBinary :: Int -> Int -> Int
bottomingBinary first second = raise# (first, second)
{-# NOINLINE bottomingBinary #-}

partialConsumer :: (Int -> Int) -> Int
partialConsumer function = function `seq` 0
{-# NOINLINE partialConsumer #-}

-- Keep the source-level partial application reachable through an opaque consumer.
bottomingPartial :: Int
bottomingPartial = partialConsumer (bottomingBinary 1)
{-# NOINLINE bottomingPartial #-}

bottomingCalled :: Int
bottomingCalled = bottomingUnary 1
{-# NOINLINE bottomingCalled #-}

bottomingTuple :: (# Int#, Double# #) -> Int
bottomingTuple (# integral, floating #) = raise# (I# integral, D# floating)
{-# NOINLINE bottomingTuple #-}

bottomingTupleCalled :: Int
bottomingTupleCalled = bottomingTuple (# 1#, 1.0## #)
{-# NOINLINE bottomingTupleCalled #-}

bottomingVoid :: (# #) -> Int
bottomingVoid _ = raise# ()
{-# NOINLINE bottomingVoid #-}

bottomingVoidCalled :: Int
bottomingVoidCalled = bottomingVoid (# #)
{-# NOINLINE bottomingVoidCalled #-}
