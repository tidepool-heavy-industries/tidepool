{-# LANGUAGE GHC2024, RankNTypes #-}
module TypedSegmentSupport where

import Data.Typeable (Typeable)
import Control.Monad.Freer (Eff)
import Control.Monad.Freer.State (State, get)

newtype SigmaNumber = SigmaNumber { sigmaNumber :: forall a. Num a => a }

data Equal a where
  EInt :: Equal Int

data Some where
  Some :: Typeable a => a -> Some

class SegmentClass a where
  segmentValue :: a
instance SegmentClass Int where
  segmentValue = 17
instance SegmentClass Double where
  segmentValue = 19

otherRow :: Eff '[State Int] Int
otherRow = get
