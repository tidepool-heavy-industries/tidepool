{-# LANGUAGE MagicHash #-}
module EnumContract where

import GHC.Exts (Int#, tagToEnum#)

data Colour = Red | Green | Blue

-- Keep the argument dynamic so GHC must preserve the operation and its family.
{-# NOINLINE colour #-}
colour :: Int# -> Colour
colour tag = tagToEnum# tag

{-# NOINLINE boolean #-}
boolean :: Int# -> Bool
boolean tag = tagToEnum# tag
