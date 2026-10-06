{-# LANGUAGE MagicHash, UnboxedSums #-}
module UnboxedSumJoin (sumJoin, signedJoin) where
import GHC.Exts (Int(I#), Int#)

sumJoin :: Bool -> Int
sumJoin choice =
  let consume :: (# Int | Int #) -> Int
      consume result = case result of
        (# value | #) -> value + 1
        (# | value #) -> value + 2
      {-# NOINLINE consume #-}
  in if choice then consume (# 5 | #) else consume (# | 7 #)

signedJoin :: Bool -> Int
signedJoin choice =
  let consume :: Int# -> Int
      consume value = I# value
      {-# NOINLINE consume #-}
  in if choice then consume (-1#) else consume 2#
