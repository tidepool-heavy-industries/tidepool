module SourceOriginalSupport (first, second) where

import qualified PublicationHelper

{-# NOINLINE first #-}
first :: Int
first = 17

{-# NOINLINE second #-}
second :: Int
second = PublicationHelper.second
