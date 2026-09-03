{-# LANGUAGE DataKinds #-}

module ShoalRunnerLeak where

import Control.Monad.Freer (Eff)
import Prelude
import Tidepool.Actors.Shoal

result :: AgentAction '[] () -> Eff '[] (Either ActionFailure ())
result = runAgentAction
