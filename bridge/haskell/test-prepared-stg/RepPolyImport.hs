{-# LANGUAGE MagicHash #-}
module RepPolyImport where

import GHC.Exts (Int#, (+#))
import GHC.Types (Int(I#))
import RepPoly (applyTo)

{-# NOINLINE importedPrim #-}
importedPrim :: Int -> Int#
importedPrim value = applyTo (\(I# n) -> n +# 1#) value

result :: Int
result = I# (importedPrim 41)
