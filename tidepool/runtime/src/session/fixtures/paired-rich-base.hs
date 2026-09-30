{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE FlexibleInstances #-}

class PublicClass a where
  type PublicFamily a
  publicClass :: a -> Int

instance PublicClass Int where
  type PublicFamily Int = Bool
  publicClass _ = 11

data PublicRecord = PublicRecord { publicField :: Int }

baseValue :: Int
baseValue = 7

retractValue :: Int
retractValue = 19
