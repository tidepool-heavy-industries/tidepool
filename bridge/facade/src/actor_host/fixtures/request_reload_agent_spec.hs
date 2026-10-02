{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import Tidepool.Duration (seconds)
import Tidepool.Effects.Core (Sleep, sleep)

data Probe = Probe { number :: Int, delay :: Int }
  deriving (Generic, FromJSON, JsonSchema)

newtype ReloadTools mode = ReloadTools { probe :: mode :- Call Probe Int }
  deriving (Generic)

agentSpec :: Member Sleep effects => AgentSpec ReloadTools effects
agentSpec = defaultSpec
  { specTools = ReloadTools
      { probe = tool "Add the installed offset after the requested delay." answer }
  }

offset :: Int
offset = 2

answer :: Member Sleep effects => Probe -> Eff effects Int
answer (Probe value pause) = do
  if pause > 0 then sleep (seconds (fromIntegral pause)) else pure ()
  pure (value + offset)
