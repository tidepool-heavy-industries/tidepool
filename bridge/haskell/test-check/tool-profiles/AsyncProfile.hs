{-# LANGUAGE DataKinds, OverloadedStrings #-}
module AsyncProfile where
import Tidepool.Agent.Contract
import Tidepool.Agent.ToolEffects (ToolSchedule (..))
import Tidepool.Effects.Core (ContextReadWrite)

-- Even an enclosing superset cannot authorize an async notebook profile.
invalid :: HaskellTool 'Asynchronous '[ContextReadWrite] '[ContextReadWrite]
invalid = haskellTool "Invalid async context notebook"
