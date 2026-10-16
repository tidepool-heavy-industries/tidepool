module ConfiguredSpec (agentSpec) where

import Tidepool.Agent.Contract (AgentSpec, NoTools)

agentSpec :: AgentSpec NoTools effects
agentSpec = missingConfiguredStartupSpec
