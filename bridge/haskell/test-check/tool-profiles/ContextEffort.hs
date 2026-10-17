{-# LANGUAGE DataKinds #-}
module ContextEffort where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Context (Reasoning (DeepSeekReasoning), DeepSeekReasoning (DeepSeekHigh), setNextReasoning)
import Tidepool.Effects.Core (ContextReadWrite)

accepted :: Eff '[ContextReadWrite] ()
accepted = setNextReasoning (DeepSeekReasoning DeepSeekHigh)
