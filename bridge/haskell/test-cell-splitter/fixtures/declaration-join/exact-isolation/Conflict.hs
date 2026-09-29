{-# LANGUAGE TypeFamilies #-}
module Conflict () where
import Common
-- The hidden F Int = Bool axiom must still prevent this declaration.
type instance F Int = Char
