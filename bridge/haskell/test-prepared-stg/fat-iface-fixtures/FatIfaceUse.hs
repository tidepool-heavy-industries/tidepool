module FatIfaceUse where

import FatFixture (fatIdentity, recA, recB)
import ThinFixture (thinIdentity)
import MissingFixture (missingIdentity)

useAll :: Int -> Int
useAll value =
  fatIdentity (recA value + recB value + thinIdentity value + missingIdentity value)
