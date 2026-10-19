{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE TypeOperators #-}

module Project.Tools (WorkspaceTools (..), tools) where

import Control.Monad.Freer (Eff, Member)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
  ( AsServerT, HaskellTools, haskellTools, KnownToolEffects, AsyncEffects
  , type (:-), Sync, Call, ReloadInput, ReloadError, ReloadOutcome, standardReload )
import Tidepool.Effects.Core (BoundWorktree, Commands, Console, Jev, Reflect)
import Tidepool.Agent.Reply (Replies)
import qualified Tidepool.Command.Tools as Command
import qualified Project.Shell as Shell
import qualified Project.ReviewTools as ReviewTools

data WorkspaceTools effects mode = WorkspaceTools
  { notebook :: HaskellTools effects mode
  , shell :: Command.ShellTools mode
  , review :: ReviewTools.ReviewTools mode
  , reload :: mode :- Sync (Call ReloadInput (Either ReloadError ReloadOutcome))
  }
  deriving (Generic)

tools ::
  ( KnownToolEffects effects, AsyncEffects effects
  , Member Commands effects, Member Console effects, Member Jev effects, Member Reflect effects
  , Member Replies effects, Member BoundWorktree effects
  ) =>
  WorkspaceTools effects (AsServerT (Eff effects))
tools = WorkspaceTools {notebook = haskellTools, shell = Shell.tools, review = ReviewTools.tools, reload = standardReload}
