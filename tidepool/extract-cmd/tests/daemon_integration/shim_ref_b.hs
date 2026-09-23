{-# LANGUAGE DataKinds #-}
module ShimRefB where

import Control.Monad.Freer (Eff)
import Tidepool.Session.Val.G1 (x)
import Tidepool.Prelude
import Tidepool.Shim
import Tidepool.Companion
import qualified Tidepool.Internal.Resume as TidepoolResume

mine :: Pinned
mine = companionVal

__result :: Int
__result = {{TURN}}
__prepared = TidepoolResume.settle (pure __result :: Eff '[] Int)
