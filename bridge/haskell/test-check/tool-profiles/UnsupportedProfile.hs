{-# LANGUAGE DataKinds, OverloadedStrings #-}
module UnsupportedProfile where
import Tidepool.Agent.Contract
import Tidepool.Agent.ToolEffects (ToolSchedule (..))
import Tidepool.Effects.Core (Commands)

-- Sync adds context authority, not an arbitrary actor effect.
invalid :: HaskellTool 'BeforeNextInference '[Commands] '[]
invalid = haskellTool "Invalid unsupported commands notebook"
