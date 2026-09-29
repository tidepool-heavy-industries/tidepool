{-# LANGUAGE TypeFamilies #-}
module DataConflict where
import Common
instance A Int where
  type Associated Int = Bool
  data AssociatedData Int = OtherIntData Char
