{-# LANGUAGE GHC2024 #-}
module SegmentProbeSupport (segmentRecord) where

import Prelude (Int)
import qualified Prelude as P
import qualified Data.Text as T
import Control.Monad.Freer (Eff, Member, send)
import Tidepool.Effects (Console(..))

segmentRecord :: Member Console effects => Int -> Eff effects ()
segmentRecord value = send (Print (T.pack (P.show value)))
