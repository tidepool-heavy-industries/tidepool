{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}

module Project.Tools (WorkspaceTools (..), tools) where

import Control.Monad.Freer (Eff, Member)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract (AsServerT)
import Tidepool.Effects.Core (BoundWorktree, Commands, Lookup, Jev, Reflect)
import Tidepool.Agent.Reply (Replies)
import qualified Tidepool.Command.Tools as Command
import qualified Project.Shell as Shell
import qualified Project.Lookup as Lookup
import qualified Project.ReviewTools as ReviewTools
import qualified Tidepool.Lookup.Tools as LookupTools

data WorkspaceTools mode = WorkspaceTools
  { shell :: Command.ShellTools mode
  , inspection :: LookupTools.LookupTools mode
  , review :: ReviewTools.ReviewTools mode
  }
  deriving (Generic)

tools ::
  ( Member Commands effects, Member Lookup effects, Member Jev effects, Member Reflect effects
  , Member Replies effects, Member BoundWorktree effects
  ) =>
  WorkspaceTools (AsServerT (Eff effects))
tools = WorkspaceTools {shell = Shell.tools, inspection = Lookup.tools, review = ReviewTools.tools}
