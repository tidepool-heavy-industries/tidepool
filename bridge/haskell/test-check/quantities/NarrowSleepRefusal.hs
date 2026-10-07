{-# LANGUAGE DataKinds #-}
module NarrowSleepRefusal where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract
import Tidepool.Duration (seconds)
import Tidepool.Effects (sleep)

narrowSpec :: AgentSpec (AsyncHaskellTools '[]) '[]
narrowSpec = defaultAsyncWorkbenchSpec

invalid :: Eff '[] ()
invalid = sleep (seconds 0)
