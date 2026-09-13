{-# LANGUAGE MagicHash #-}
{-# LANGUAGE UnboxedTuples #-}

module M3Vertical where

import Data.List (foldl', reverse)
import GHC.Exts (Double#, Int#, Int(I#), double2Int#, (+#))

data Box = Box !Int

result :: Int
result = 42

tupleArgumentCall :: (# Int#, Double# #) -> Int#
tupleArgumentCall (# count, ratio #) = count +# double2Int# ratio
{-# NOINLINE tupleArgumentCall #-}

tupleArgumentUse :: (# Int#, Double# #) -> Int
tupleArgumentUse (# count, ratio #) = I# (tupleArgumentCall (# count, ratio #))

tupleArgumentResult :: Int
tupleArgumentResult = tupleArgumentUse (# 41#, 1.0## #)

-- Keep a call to a package function so the projected import contract comes
-- from GHC's interface entry information.
importedReverse :: [Int] -> [Int]
importedReverse values = reverse values
{-# NOINLINE importedReverse #-}

-- Keep two semantic zero-width parameters in the prepared STG so projection
-- must give them distinct ValueIds even when GHC reuses its wired-in binder.
voidParameterPair :: (# #) -> (# #) -> Int# -> Int#
voidParameterPair _ _ value = value +# 1#
{-# NOINLINE voidParameterPair #-}

voidParameterResult :: Int
voidParameterResult = I# (voidParameterPair (# #) (# #) 41#)

-- A lone unboxed variable is StgApp with no arguments, but it is already a
-- value and must return directly rather than enter a closure.
returnUnboxedArgument :: Int# -> Int#
returnUnboxedArgument value = value
{-# NOINLINE returnUnboxedArgument #-}

returnUnboxedArgumentResult :: Int
returnUnboxedArgumentResult = I# (returnUnboxedArgument 42#)

-- The case binder, rather than the callee's source type, demands the result
-- representation of this application.
demandedCallee :: Int# -> Int#
demandedCallee value = value +# 1#
{-# NOINLINE demandedCallee #-}

demandedCaseResult :: Int
demandedCaseResult = case demandedCallee 41# of value -> I# value

-- STG erases the polymorphic type application, so this call supplies the
-- function and its Int# argument even though the declared source arrow has
-- one value argument. Its result must come from the enclosing demand.
polymorphicIdentity :: a -> a
polymorphicIdentity value = value
{-# NOINLINE polymorphicIdentity #-}

polymorphicIdentityResult :: Int
polymorphicIdentityResult = I# (polymorphicIdentity demandedCallee 41#)

-- The qualified top reference must remain visible even though a local binder
-- has the same unit/module/occurrence rendering.
sameOccurrenceTop :: Int# -> Int#
sameOccurrenceTop value = value +# 1#
{-# NOINLINE sameOccurrenceTop #-}

sameOccurrenceResult :: Int
sameOccurrenceResult =
  let sameOccurrenceTop = 41#
  in I# (M3Vertical.sameOccurrenceTop sameOccurrenceTop)

entry :: Int -> Box
entry count = go count (foldl' (+) 0 [1, 2, 3])
  where
    go 0 value = Box value
    go remaining value = go (remaining - 1) (value + 1)
