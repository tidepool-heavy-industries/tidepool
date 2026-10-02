{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract

newtype Probe = Probe { number :: Int }
  deriving (Generic, FromJSON, JsonSchema)

newtype BrowserTools mode = BrowserTools { probe :: mode :- Call Probe Int }
  deriving (Generic)

agentSpec :: AgentSpec BrowserTools effects
agentSpec = defaultSpec
  { specTools = BrowserTools
      { probe = tool "Add two to the supplied number." answer }
  }

answer :: Probe -> Eff effects Int
answer (Probe value) = pure (value + 2)
