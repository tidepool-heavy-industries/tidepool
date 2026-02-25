{-# LANGUAGE MagicHash, UnboxedTuples #-}
module CmpTest where
import GHC.Exts
import Tidepool.Prelude

-- Simulate the exact byte length logic for ':' (0x3A)
result :: Int
result = 
  let r# = 58## -- 0x3A
      c# = clz8# (and# (not# r#) 255##)
      c1 = word2Int# c#
      y = xorI# c1 (c1 <=# 0#)
  in I# y
