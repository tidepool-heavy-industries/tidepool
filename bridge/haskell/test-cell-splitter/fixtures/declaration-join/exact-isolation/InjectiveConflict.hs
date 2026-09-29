{-# LANGUAGE TypeFamilies #-}
module InjectiveConflict where
import Common
type instance J Double = Bool
