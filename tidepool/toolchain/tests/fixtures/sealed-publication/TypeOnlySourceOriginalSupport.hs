{-# LANGUAGE TypeFamilies #-}
module SourceOriginalSupport (first, second) where

import qualified PublicationHelper

type family SupportResult a where
  SupportResult PublicationHelper.Witness = Int

{-# NOINLINE first #-}
first :: SupportResult PublicationHelper.Witness
first = 17

{-# NOINLINE second #-}
second :: Int
second = 23
