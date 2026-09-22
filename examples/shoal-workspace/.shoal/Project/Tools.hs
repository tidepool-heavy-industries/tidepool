{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}

module Project.Tools (WorkspaceTools (..), tools) where

import Control.Monad.Freer (Eff, Member)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract (AsServerT)
import Tidepool.Effects.Core (Commands, Jev, Reflect)
import qualified Tidepool.Command.Tools as Command
import qualified Project.Shell as Shell

data WorkspaceTools mode = WorkspaceTools
  { shell :: Command.ShellTools mode
  }
  deriving (Generic)

tools ::
  (Member Commands effects, Member Jev effects, Member Reflect effects) =>
  WorkspaceTools (AsServerT (Eff effects))
tools = WorkspaceTools {shell = Shell.tools}
