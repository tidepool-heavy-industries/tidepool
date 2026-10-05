{-# LANGUAGE FlexibleInstances, TypeFamilies #-}
module MetadataLoadedFamily where

import GHC.Exts (IsList(..))

instance IsList (Maybe Int) where
  type Item (Maybe Int) = Bool
  fromList _ = Nothing
  toList _ = []
