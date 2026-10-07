{-# LANGUAGE DataKinds #-}
module NarrowScopeRefusal where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract
import Tidepool.Scope (ScopeOutcome, withScope)

narrowSpec :: AgentSpec (AsyncHaskellTools '[]) '[]
narrowSpec = defaultAsyncWorkbenchSpec

-- A supplied pure notebook gains no authority from the default actor row.
invalid :: Eff '[] (ScopeOutcome ())
invalid = withScope (\_ -> pure ())
