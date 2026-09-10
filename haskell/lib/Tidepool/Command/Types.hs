{-# LANGUAGE OverloadedStrings #-}
module Tidepool.Command.Types
  ( Command (..), Job (..), Memory (..), bashCommand, argv, describe
  , withMemory, inDirectory, withEnvironment, withArguments, withStdin, withTerminal
  ) where

import Data.Text (Text)
import Tidepool.Effects.Core (CommandSpec (..), CommandInput (..))

-- Internal constructors; Tidepool.Command exposes descriptions and opaque jobs.
newtype Command = Command CommandSpec deriving (Eq, Show)
newtype Job = Job Text deriving (Eq, Show)
data Memory = MiB Int | GiB Int deriving (Eq, Show)

argv :: [Text] -> Command
argv args = Command (CommandSpec args Nothing [] (256 * 1024 * 1024) ClosedInput)

describe :: Command -> CommandSpec
describe (Command spec) = spec

bashCommand :: Text -> Command
bashCommand script = argv ["bash", "--noprofile", "--norc", "-c", script, "shoal-bash"]

withMemory :: Memory -> Command -> Command
withMemory size (Command spec) = Command spec { commandMemory = bytes size }
  where
    bytes (MiB amount) = scale (1024 * 1024) amount
    bytes (GiB amount) = scale (1024 * 1024 * 1024) amount
    scale unit amount
      | amount <= 0 || amount > maxBound `div` unit = error "command memory must be positive and fit Int"
      | otherwise = unit * amount

inDirectory :: Text -> Command -> Command
inDirectory path (Command spec) = Command spec { commandDirectory = Just path }

-- | Override the named entries while preserving other environment customizations.
withEnvironment :: [(Text, Text)] -> Command -> Command
withEnvironment values (Command spec) = Command spec
  { commandEnvironment = values ++ filter (\(key, _) -> key `notElem` map fst values) (commandEnvironment spec)
  }

-- | Append actual arguments. In a bash command these become $1, $2, etc.
withArguments :: [Text] -> Command -> Command
withArguments args (Command spec) = Command spec { commandArgv = commandArgv spec ++ args }

withStdin :: Command -> Command
withStdin (Command spec) = Command spec { commandInput = PipeInput }

withTerminal :: Command -> Command
withTerminal (Command spec) = Command spec { commandInput = TerminalInput }
