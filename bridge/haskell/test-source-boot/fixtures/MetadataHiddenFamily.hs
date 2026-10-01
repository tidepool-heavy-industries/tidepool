{-# LANGUAGE FlexibleInstances, TypeFamilies #-}
module MetadataHiddenFamily where

import GHC.Exts (IsList(..))

instance IsList (Maybe Int) where
  type Item (Maybe Int) = Bool
  fromList _ = Nothing
  toList _ = []
