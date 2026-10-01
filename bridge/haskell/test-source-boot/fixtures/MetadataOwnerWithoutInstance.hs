module MetadataOwner where

class Available a where
  available :: a -> Int
