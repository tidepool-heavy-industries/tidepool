{-# LANGUAGE GHC2024 #-}
module SegmentProbeSupport (record) where

import Prelude (Int)
import qualified Prelude as P
import qualified Data.Text as T
import Control.Monad.Freer (Eff, Member, send)
import Tidepool.Effects (Console(..))

record :: Member Console effects => Int -> Eff effects ()
record value = send (Print (T.pack (P.show value)))
