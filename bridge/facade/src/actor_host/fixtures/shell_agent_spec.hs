{-# LANGUAGE FlexibleContexts #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract (AgentSpec (..), defaultSpec)
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Shell

agentSpec :: Member Cmd.Commands effects => AgentSpec Shell.ShellTools effects
agentSpec = defaultSpec {specTools = Shell.tools}
