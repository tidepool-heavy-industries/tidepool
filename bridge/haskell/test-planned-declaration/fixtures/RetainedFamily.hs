{-# LANGUAGE TypeFamilies #-}
module RetainedFamily where

type family OldPayload a
type instance OldPayload Int = Bool
