module SourceOriginalSupport (first, second) where

import qualified PublicationHelper
import qualified NarrowingSupport

{-# NOINLINE first #-}
first :: Int
first = 17

{-# NOINLINE second #-}
second :: Int
second = PublicationHelper.second + NarrowingSupport.second
