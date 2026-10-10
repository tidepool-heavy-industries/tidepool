{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Shell

data RetainedShellTools effects mode = RetainedShellTools
  { shell :: Shell.ShellTools mode
  , notebook :: HaskellTools effects mode
  }
  deriving (Generic)

agentSpec
  :: (KnownToolEffects effects, AsyncEffects effects, Member Cmd.Commands effects)
  => AgentSpec (RetainedShellTools effects) effects
agentSpec = defaultSpec
  { specTools = RetainedShellTools
      { shell = Shell.tools
      , notebook = haskellTools
      }
  }
