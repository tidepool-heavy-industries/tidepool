{-# LANGUAGE RankNTypes, MagicHash #-}
module CellDisplayExternal (Unknown(..), Shown(..), PolyField, ByteArrayField, Display(..), Generic(..)) where

import GHC.Exts (ByteArray#)

data Unknown = Unknown
data Shown = Shown Int deriving Show
type PolyField = forall a. a -> a
type ByteArrayField = ByteArray#

class Display a where foreignDisplay :: a -> ()
class Generic a where foreignGeneric :: a -> ()
