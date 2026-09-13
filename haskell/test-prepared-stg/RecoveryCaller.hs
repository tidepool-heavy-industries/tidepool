module RecoveryCaller where

import Prelude (Int, fst)
import RecoveryHome (homeValue)

caller :: Int
caller = homeValue (fst (1, ()))
