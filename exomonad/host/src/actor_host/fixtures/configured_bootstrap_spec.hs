module ConfiguredSpec (agentSpec) where

import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)

agentSpec :: AgentSpec NoTools effects
agentSpec = defaultSpec
