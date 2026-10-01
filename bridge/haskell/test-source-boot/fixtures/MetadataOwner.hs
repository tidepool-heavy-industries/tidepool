module MetadataOwner where

class Available a where
  available :: a -> Int

instance Available Int where
  available = id
