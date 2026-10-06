module RecoveredBaseCaller where

import Data.List.NonEmpty (NonEmpty)
import qualified Data.List.NonEmpty as NonEmpty

{-# OPAQUE caller #-}
caller :: NonEmpty a -> [a]
caller = NonEmpty.toList
