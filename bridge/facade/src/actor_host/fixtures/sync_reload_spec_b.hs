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

data SearchInput = SearchInput { query :: Text } deriving (Generic)
instance FromJSON SearchInput
instance JsonSchema SearchInput

data WorkshopTools mode = WorkshopTools
  { webSearch :: mode :- Sync (Call SearchInput Text)
  , activateDraft :: mode :- Sync (Call ReloadInput (Either ReloadError ReloadOutcome))
  , haskell :: mode :- Sync (HaskellCell PersonaEffects)
  } deriving (Generic)
agentSpec :: (Subset PersonaEffects (SyncEffects effects), Members '[FsRead, FsWrite] effects)
  => AgentSpec WorkshopTools effects
agentSpec = defaultSpec
  { specTools = WorkshopTools
      { webSearch = presentWith id (syncTool "Search the workshop fixture."
          (\request -> pure ("B:" <> query request <> ":" <> T.pack (show (Work.answerFn 1)))))
      , activateDraft = standardReload
      , haskell = haskellTool "Run the workshop notebook."
      }
  , initialContext = Just Orientation.startup
  }
