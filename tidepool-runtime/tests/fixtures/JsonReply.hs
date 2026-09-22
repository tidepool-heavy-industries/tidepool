{-# LANGUAGE DataKinds, TypeApplications, OverloadedStrings #-}
module JsonReply where

import Control.Monad.Freer (Eff)
import Tidepool.Aeson.Value (Value)
import Tidepool.Effects.Core (RunLLMTurn, runLLMTurn)
import Tidepool.Internal.Resume (settle, resumeLifted)

result :: Eff '[RunLLMTurn] Value
result = runLLMTurn @Value "return nested JSON"

__prepared = settle result
__resume q x = settle (resumeLifted q x)
