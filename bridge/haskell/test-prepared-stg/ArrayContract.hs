{-# LANGUAGE MagicHash #-}
{-# LANGUAGE UnboxedTuples #-}

module ArrayContract where

import GHC.Exts
  ( Int#, RealWorld, SmallMutableArray#, State#
  , newSmallArray#, readSmallArray#, writeSmallArray# )

newArrayContract
  :: Int# -> Int -> State# RealWorld
  -> (# State# RealWorld, SmallMutableArray# RealWorld Int #)
newArrayContract length initial state = newSmallArray# length initial state
{-# NOINLINE newArrayContract #-}

readArrayContract
  :: SmallMutableArray# RealWorld Int -> Int# -> State# RealWorld
  -> (# State# RealWorld, Int #)
readArrayContract array index state = readSmallArray# array index state
{-# NOINLINE readArrayContract #-}

writeArrayContract
  :: SmallMutableArray# RealWorld Int -> Int# -> Int -> State# RealWorld
  -> State# RealWorld
writeArrayContract array index value state = writeSmallArray# array index value state
{-# NOINLINE writeArrayContract #-}
