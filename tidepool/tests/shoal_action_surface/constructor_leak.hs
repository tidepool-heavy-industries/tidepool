{-# LANGUAGE DataKinds #-}

module ShoalConstructorLeak where

import Prelude
import Tidepool.Actors.Shoal

result :: AgentAction '[] ()
result = AgentAction (pure (Right ()))
