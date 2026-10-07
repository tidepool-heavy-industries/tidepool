{-# LANGUAGE GHC2024 #-}
module SegmentProbeSupport
  ( segmentRecord
  , carrierBaseline
  , lateCarrierIncrement
  , lateCarrierDouble
  , lateCarrierNegate
  ) where

import Prelude (Int)
import qualified Prelude as P
import qualified Data.Text as T
import Control.Monad.Freer (Eff, Member, send)
import Tidepool.Effects (Console(..))

segmentRecord :: Member Console effects => Int -> Eff effects ()
segmentRecord value = send (Print (T.pack (P.show value)))

-- Keep reuse-test demand as original call edges rather than inlining or
-- specialization. The uncalled negate helper distinguishes partial selection.
{-# OPAQUE carrierBaseline #-}
carrierBaseline :: Int -> Int
carrierBaseline value = value

{-# OPAQUE lateCarrierIncrement #-}
lateCarrierIncrement :: Int -> Int
lateCarrierIncrement value = value P.+ 1

{-# OPAQUE lateCarrierDouble #-}
lateCarrierDouble :: Int -> Int
lateCarrierDouble value = value P.* 2 P.+ 1

{-# OPAQUE lateCarrierNegate #-}
lateCarrierNegate :: Int -> Int
lateCarrierNegate value = P.negate value
