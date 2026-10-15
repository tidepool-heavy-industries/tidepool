{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | A reusable researcher with Web tools and no notebook or general reply effect.
module Project.Researcher
  ( ResearchEffects
  , ResearchTools (ResearchTools)
  , SearchInput (..)
  , OpenInput (..)
  , Citation (..)
  , Report (..)
  , FollowUp (..)
  , searchHandler
  , openHandler
  , researcherSpec
  ) where

import Prelude
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON (toJSON), Value)
import Tidepool.Agent.Contract
  ( AgentSpec (..), Call, JsonSchema, Sync, type (:-)
  , defaultSpec, presentJson, presentWith, syncTool
  )
import Tidepool.Effects.Core (Web, WebError, webSearchRaw, webExtractRaw)
import Tidepool.Inspection.Display (Display, WorkbenchDisplay (..))

type ResearchEffects = '[Web]

data SearchInput = SearchInput { query :: Text, limit :: Int }
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)

newtype OpenInput = OpenInput { url :: Text }
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)

data Citation = Citation { sourceUrl :: Text, sourceReference :: Text }
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)

data Report = Report { question :: Text, summary :: Text, sources :: [Citation] }
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)

data FollowUp = FollowUp { answer :: Text, citations :: [Text] }
  deriving (Eq, Show, Generic, FromJSON, ToJSON, JsonSchema)

instance Display Citation
instance Display Report
instance Display FollowUp
instance WorkbenchDisplay Report where
  workbenchDisplay report = (presentJson report, False)
instance WorkbenchDisplay FollowUp where
  workbenchDisplay followUp = (presentJson followUp, False)

data ResearchTools mode = ResearchTools
  { search :: mode :- Sync (Call SearchInput Value)
  , open :: mode :- Sync (Call OpenInput Value)
  }
  deriving (Generic)

-- The domain owner validates the query and budget and retains typed failures.
searchHandler :: Member Web effects
  => SearchInput -> Eff effects (Either WebError Value)
searchHandler (SearchInput text count) = webSearchRaw text count [] []

openHandler :: Member Web effects
  => OpenInput -> Eff effects (Either WebError Value)
openHandler (OpenInput target) = webExtractRaw target

-- | Pass this concrete value directly to defaultSpawnOptions. Search returns
-- title/URL/content rows; open returns URL/reference/content evidence. The
-- active request supplies its own reply tool and result schema.
researcherSpec :: AgentSpec ResearchTools ResearchEffects
researcherSpec = defaultSpec
  { specTools = ResearchTools
      { search = presentWith presentJson $ syncTool
          "Search public pages. Use a nonempty query and limit 1–20; retain the returned URLs."
          (fmap toJSON . searchHandler)
      , open = presentWith presentJson $ syncTool
          "Open a public page. Preserve its URL, reference and content as source evidence."
          (fmap toJSON . openHandler)
      }
  }
