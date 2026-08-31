module ShimRefB where

import Tidepool.Session.Val.G1 (x)
import Tidepool.Prelude
import Tidepool.Shim
import Tidepool.Companion

mine :: Pinned
mine = companionVal

__result :: Int
__result = {{TURN}}
