{-# LANGUAGE QuasiQuotes #-}
-- | Phase-0 spike: does a QQ splice survive the extract pipeline under
-- backend = noBackend?
module SpikeUse where

import UpperQQ (upper)

spike_upper :: String
spike_upper = [upper|hello|]

litd :: Double
litd = -2.5

addd :: Double
addd = 1.5 + 2.25
