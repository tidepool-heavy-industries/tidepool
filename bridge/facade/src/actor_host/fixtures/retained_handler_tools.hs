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

data SpecTools effects mode = SpecTools
  { probe :: mode :- Call Probe Text
  , notebook :: HaskellTools effects mode
  }
  deriving (Generic)

tools :: (KnownToolEffects effects, AsyncEffects effects) => SpecTools effects (AsServerT (Eff effects))
tools = SpecTools
  { probe = presentWith id $ tool "Answer one fixed question about a topic." answer
  , notebook = haskellTools
  }
  where
    answer _ = pure "HANDLER_GENERATION"
