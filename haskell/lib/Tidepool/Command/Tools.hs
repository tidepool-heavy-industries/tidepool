{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Tidepool.Command.Tools (ShellTools (..), tools) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd

-- Result presentation and retained-job recovery belong to the command effect.
data ShellTools mode = ShellTools
  { bash :: mode :- RawCall Text
  } deriving Generic

tools :: Member Cmd.Commands effects => ShellTools (AsServerT (Eff effects))
tools = ShellTools
  { bash = rawTool
      "Run literal Bash in this actor's workspace. Output is displayed directly. Commands use 256 MiB and wait up to 30 seconds. For larger memory, arguments, PTY, or composition use Cmd in Haskell. Long-running or oversized results retain a jobN binding for Haskell inspection; never rerun to recover output."
      (\script -> Cmd.run (Cmd.bashCommand script) >> pure "")
  }
