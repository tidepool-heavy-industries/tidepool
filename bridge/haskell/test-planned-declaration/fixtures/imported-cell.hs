{-# LANGUAGE GADTs, TypeFamilies, FlexibleInstances #-}
import qualified Foreign as Named (Box(..))
data Box where
  Box :: { common :: Int } -> Box
  Taken :: Int -> Box
(<+>) :: Int -> Int -> Int
left <+> right = left - right
id :: a -> a
id value = value
let value = Box 42
let qualifiedValue = Named.Box 17
let implicitQualifiedValue = Foreign.Box 19
let remaining = Keep
let originalConstructor = Taken 23
let foreignRecord = ForeignRecord 5 7
common value + untouched foreignRecord + (8 <+> 3) + (8 Foreign.<+> 3) + id 2
