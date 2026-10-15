{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where
import Control.Monad.Freer (Members)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (FsRead, FsWrite)
import Tidepool.Aeson (FromJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import Persona.Spec (PersonaEffects)
import qualified Persona.Orientation as Orientation
import qualified Project.Work as Work

data WorkshopTools mode = WorkshopTools
  { haskell :: mode :- Sync (HaskellCell PersonaEffects)
  , applyDraft :: mode :- Sync (Call ReloadInput (Either ReloadError ReloadOutcome))
  } deriving (Generic)
agentSpec :: (Subset PersonaEffects (SyncEffects effects), Members '[FsRead, FsWrite] effects)
  => AgentSpec WorkshopTools effects
agentSpec = defaultSpec
  { specTools = WorkshopTools
      { haskell = haskellTool "Run the workshop notebook."
      , applyDraft = standardReload
      }
  , initialContext = Just Orientation.startup
  }
