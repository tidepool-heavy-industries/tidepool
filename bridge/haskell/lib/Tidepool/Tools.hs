{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}

-- | Default self-hosted tools for workspaces without an explicit tools record.
module Tidepool.Tools (Tools (..), tools) where

import Control.Monad.Freer (Eff, Member)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract (AsServerT)
import Tidepool.Effects.Core (Commands, Console)
import qualified Tidepool.Command.Tools as Command

data Tools mode = Tools
  { shell :: Command.ShellTools mode
  } deriving (Generic)

tools :: (Member Commands effects, Member Console effects) => Tools (AsServerT (Eff effects))
tools = Tools {shell = Command.tools}
