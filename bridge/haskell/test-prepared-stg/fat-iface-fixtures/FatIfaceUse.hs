module FatIfaceUse where

import FatFixture (fatIdentity, recA, recB, privateCaller, privateDiamond)
import ThinFixture (thinIdentity)
import MissingFixture (missingIdentity)

useAll :: Int -> Int
useAll value =
  fatIdentity (privateCaller value + recA value + recB value + thinIdentity value + missingIdentity value)
