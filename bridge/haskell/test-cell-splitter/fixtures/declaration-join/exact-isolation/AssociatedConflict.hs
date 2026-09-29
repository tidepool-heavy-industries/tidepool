{-# LANGUAGE TypeFamilies #-}
{-# OPTIONS_GHC -Wno-missing-methods #-}
-- Omit the associated data instance to isolate the type equation conflict.
module AssociatedConflict where
import Common
instance A Int where
  type Associated Int = Char
