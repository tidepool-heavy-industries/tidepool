{-# LANGUAGE DataKinds #-}
module ShimRefA where

import Control.Monad.Freer (Eff)
import Tidepool.Session.Val.G1 (x)
import Tidepool.Prelude
import Tidepool.Shim
import Tidepool.Companion
import qualified Tidepool.Internal.Resume as TidepoolResume

__result :: Int
__result = {{TURN}}
__prepared = TidepoolResume.settle (pure __result :: Eff '[] Int)
