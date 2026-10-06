{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeOperators #-}
module QuotedAgentSpec (agentSpec) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import QuotedProvider (capture)

newtype Probe = Probe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

newtype Tools mode = Tools { probe :: mode :- Call Probe Text }
  deriving Generic

quotedDescription :: Text
quotedDescription = Text.pack (show ([capture|{quotation-input}|] :: Int))

agentSpec :: AgentSpec Tools effects
agentSpec = defaultSpec
  { specTools = Tools
      { probe = presentWith id $ tool quotedDescription (\_ -> pure "quoted original") }
  }
