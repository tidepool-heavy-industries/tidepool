{-# LANGUAGE MagicHash #-}
{-# LANGUAGE UnboxedTuples #-}

module ByteArrayContract where

import GHC.Exts
  ( ByteArray#, Int#, MutableByteArray#, RealWorld, State#, Word8#
  , getSizeofMutableByteArray#, indexIntArray#, indexWord8Array#
  , newByteArray#, readIntArray#, readWord8Array#, sizeofByteArray#
  , unsafeFreezeByteArray#, writeIntArray#, writeWord8Array# )

newByteContract :: Int# -> State# RealWorld
  -> (# State# RealWorld, MutableByteArray# RealWorld #)
newByteContract size state = newByteArray# size state
{-# NOINLINE newByteContract #-}

freezeByteContract :: MutableByteArray# RealWorld -> State# RealWorld
  -> (# State# RealWorld, ByteArray# #)
freezeByteContract array state = unsafeFreezeByteArray# array state
{-# NOINLINE freezeByteContract #-}

sizeofByteContract :: ByteArray# -> Int#
sizeofByteContract array = sizeofByteArray# array
{-# NOINLINE sizeofByteContract #-}

getSizeofMutableByteContract :: MutableByteArray# RealWorld -> State# RealWorld
  -> (# State# RealWorld, Int# #)
getSizeofMutableByteContract array state = getSizeofMutableByteArray# array state
{-# NOINLINE getSizeofMutableByteContract #-}

readWord8Contract :: MutableByteArray# RealWorld -> Int# -> State# RealWorld
  -> (# State# RealWorld, Word8# #)
readWord8Contract array index state = readWord8Array# array index state
{-# NOINLINE readWord8Contract #-}

writeWord8Contract :: MutableByteArray# RealWorld -> Int# -> Word8# -> State# RealWorld
  -> State# RealWorld
writeWord8Contract array index value state = writeWord8Array# array index value state
{-# NOINLINE writeWord8Contract #-}

indexWord8Contract :: ByteArray# -> Int# -> Word8#
indexWord8Contract array index = indexWord8Array# array index
{-# NOINLINE indexWord8Contract #-}

readIntContract :: MutableByteArray# RealWorld -> Int# -> State# RealWorld
  -> (# State# RealWorld, Int# #)
readIntContract array index state = readIntArray# array index state
{-# NOINLINE readIntContract #-}

writeIntContract :: MutableByteArray# RealWorld -> Int# -> Int# -> State# RealWorld
  -> State# RealWorld
writeIntContract array index value state = writeIntArray# array index value state
{-# NOINLINE writeIntContract #-}

indexIntContract :: ByteArray# -> Int# -> Int#
indexIntContract array index = indexIntArray# array index
{-# NOINLINE indexIntContract #-}
