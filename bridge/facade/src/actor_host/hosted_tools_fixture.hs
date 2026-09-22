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

data EchoInput = EchoInput {text :: Text, copies :: Int}
  deriving (Generic, FromJSON, JsonSchema)

data TestTools mode = TestTools
  { rawEcho :: mode :- RawCall Text,
    repeatText :: mode :- Call EchoInput Text,
    launch :: mode :- Call Shell.Execute Text,
    followProcess :: mode :- Call Shell.WriteInput Text,
    readLog :: mode :- Call Shell.ReadOutput Text
  }
  deriving (Generic)

tools :: (Member Cmd.Commands effects) => TestTools (AsServerT (Eff effects))
tools =
  TestTools
    { rawEcho = rawTool "Echo literal input." $ \input ->
        if input == "fail"
          then error "expected raw handler failure"
          else pure ("frozen:" <> input),
      repeatText = tool "Repeat supplied text." $ \EchoInput {text = value, copies = count} ->
        pure (T.replicate count value),
      launch = tool "Launch with project postprocessing." $ \input -> Shell.execute input >> pure "handler continued",
      followProcess = tool "Continue the retained command." Shell.writeInput,
      readLog = tool "Read retained diagnostics." Shell.readRetained
    }
