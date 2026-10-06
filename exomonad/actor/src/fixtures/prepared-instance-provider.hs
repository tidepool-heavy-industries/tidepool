{-# LANGUAGE OverloadedStrings #-}
module PreparedInstanceProvider (instanceResult) where

import Data.Text (Text)

class ProbeResult a where
  probeResult :: a -> Text

data Probe = Probe

instance ProbeResult Probe where
  probeResult _ = "INSTANCE_RESULT"

instanceResult :: Text
instanceResult = probeResult Probe
