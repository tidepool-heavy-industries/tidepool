{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module Project.Tools where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Shell
import Tidepool.Effects.Core (FsRead, FsWrite)
import qualified Tidepool.Effects.Authored as Files

data EchoInput = EchoInput {text :: Text, copies :: Int}
  deriving (Generic, FromJSON, JsonSchema)

data TestTools effects mode = TestTools
  { notebook :: HaskellTools effects mode,
    rawEcho :: mode :- RawCall Text,
    repeatText :: mode :- Call EchoInput Text,
    launch :: mode :- Call Shell.Execute Shell.CommandToolResult,
    followProcess :: mode :- Call Shell.WriteInput Shell.CommandToolResult,
    readLog :: mode :- Call Shell.ReadOutput Shell.CommandToolResult
  }
  deriving (Generic)

tools :: (KnownToolEffects effects, AsyncEffects effects, Member Cmd.Commands effects, Member FsRead effects, Member FsWrite effects)
  => TestTools effects (AsServerT (Eff effects))
tools =
  TestTools
    { notebook = haskellTools,
      rawEcho = presentWith id $ rawTool "Echo literal input." $ \input ->
        if input == "fail"
          then error "expected raw handler failure"
          else pure ("frozen:" <> input),
      repeatText = presentWith id $ tool "Repeat supplied text." $ \EchoInput {text = value, copies = count} -> do
        Files.appendFile ".exomonad/state/handler-calls" "x" >>= either (error . show) pure
        pure (T.replicate count value),
      launch = presentWith presentDisplay $ tool "Launch with project postprocessing." $ \input -> do
        output <- Shell.execute input
        pure (output {Shell.presentation = Shell.presentation output <> "\nhandler continued"}),
      followProcess = presentWith presentDisplay $ tool "Continue the retained command." Shell.writeInput,
      readLog = presentWith presentDisplay $ tool "Read retained diagnostics." Shell.readRetained
    }

agentSpec :: (KnownToolEffects effects, AsyncEffects effects, Member Cmd.Commands effects, Member FsRead effects, Member FsWrite effects)
  => AgentSpec (TestTools effects) effects
agentSpec = defaultSpec {specTools = tools}
