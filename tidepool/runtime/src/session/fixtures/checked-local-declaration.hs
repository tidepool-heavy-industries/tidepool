{-# LANGUAGE TypeFamilies #-}
data LocalBox = LocalBox { localNumber :: Int }
class LocalClass a where
  localValue :: a -> Int
instance LocalClass LocalBox where
  localValue = localNumber
type family LocalPayload (flag :: Bool) where
  LocalPayload 'True = LocalBox
makeLocal :: Int -> LocalPayload 'True
makeLocal = LocalBox
historical :: Int
historical = 40
let historical = 41
let local = (makeLocal historical :: LocalBox)
(localValue local, Tidepool.Session.Lib.G1.historical)
