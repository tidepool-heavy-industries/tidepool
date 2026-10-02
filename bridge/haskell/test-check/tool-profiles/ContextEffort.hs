{-# LANGUAGE DataKinds #-}
module ContextEffort where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Context (ForkEffort (High), setNextEffort)
import Tidepool.Effects.Core (ContextReadWrite)

accepted :: Eff '[ContextReadWrite] ()
accepted = setNextEffort High
