{-# LANGUAGE DataKinds, TypeApplications #-}
module ReloadAsync where
import Prelude
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Reload)

-- Even a tool that does not execute Reload cannot declare its async authority.
bad :: AgentSpec (HaskellTools '[Reload]) '[Reload]
bad = defaultSpec { specTools = haskellTools }
