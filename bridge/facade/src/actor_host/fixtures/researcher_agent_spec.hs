{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec, invalidSpec) where
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import Tidepool.Actors.Exomonad (ActorEffects)
import Tidepool.Effects.Core (Web)
agentSpec :: AgentSpec WorkbenchTools ActorEffects
agentSpec = defaultWorkbenchSpec

data ScalarTools mode = ScalarTools
  { scalar :: mode :- Sync (Call Int Int) } deriving Generic
invalidSpec :: AgentSpec ScalarTools '[Web]
invalidSpec = defaultSpec
  { specTools = ScalarTools
      { scalar = presentWith presentJson $ syncTool "Invalid scalar input." pure }
  }
