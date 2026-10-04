{-# LANGUAGE TypeFamilies, GADTs, StandaloneDeriving #-}
data LocalBox where
  LocalBox :: { localNumber :: Int } -> LocalBox
deriving instance Eq LocalBox
deriving instance Show LocalBox
class LocalClass a where
  localValue :: a -> Int
instance LocalClass LocalBox where
  localValue = localNumber
type family LocalClosed (flag :: Bool) where
  LocalClosed 'True = LocalBox
type family LocalPayload (flag :: Bool)
type instance LocalPayload 'True = LocalClosed 'True
makeLocal :: Int -> LocalPayload 'True
makeLocal = LocalBox
historical :: Int
historical = 40
let historical = 41
let local = (makeLocal historical :: LocalBox)
(localValue local, {{DECLARATION_MODULE}}.historical, local == LocalBox historical, P.show local)
