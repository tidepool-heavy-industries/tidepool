{-# LANGUAGE TypeFamilies #-}
module FamilyOnly () where
import Common
type instance F Double = Char
data family Standalone a
data instance Standalone Int = StandaloneInt Int
