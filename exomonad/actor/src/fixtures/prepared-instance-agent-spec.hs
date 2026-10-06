{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module PreparedInstanceAgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import Tidepool.Duration (milliseconds)
import Tidepool.Effects.Core (Sleep, sleep)
import PreparedInstanceProvider (instanceResult)

newtype Probe = Probe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

newtype Tools mode = Tools { probe :: mode :- Call Probe Text }
  deriving Generic

agentSpec :: Member Sleep effects => AgentSpec Tools effects
agentSpec = defaultSpec
  { specTools = Tools
      { probe = presentWith id $ tool "source instance and ordered effect probe" $ \_ -> do
          sleep (milliseconds 19)
          pure instanceResult
      }
  }
