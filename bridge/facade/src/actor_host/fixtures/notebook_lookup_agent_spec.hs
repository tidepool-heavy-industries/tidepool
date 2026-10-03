{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}

module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Lookup)
import qualified Tidepool.Lookup.Tools as LookupTools

data CampaignTools effects mode = CampaignTools
  { notebook :: HaskellTools effects mode
  , inspection :: LookupTools.LookupTools mode
  }
  deriving (Generic)

agentSpec ::
  ( KnownToolEffects effects
  , AsyncEffects effects
  , Member Lookup effects
  ) =>
  AgentSpec (CampaignTools effects) effects
agentSpec = defaultSpec
  { specTools = CampaignTools
      { notebook = haskellTools
      , inspection = LookupTools.tools
      }
  }
