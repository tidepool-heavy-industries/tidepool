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
let local = makeLocal (41 :: Int)
localValue local
