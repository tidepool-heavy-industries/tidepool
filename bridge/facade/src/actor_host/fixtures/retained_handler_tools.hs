{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module Project.Tools (SpecTools (..), tools) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract

newtype Probe = Probe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

newtype SpecTools mode = SpecTools { probe :: mode :- Call Probe Text }
  deriving (Generic)

tools :: SpecTools (AsServerT (Eff effects))
tools = SpecTools
  { probe = presentWith id $ tool "Answer one fixed question about a topic." answer }
  where
    answer _ = pure "HANDLER_GENERATION"
