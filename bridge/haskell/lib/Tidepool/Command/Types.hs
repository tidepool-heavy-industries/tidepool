{-# LANGUAGE OverloadedStrings #-}
module Tidepool.Command.Types
  ( Command (..), Job (..), Memory (..), bashCommand, argv, describe, withSource
  , withMemory, inDirectory, withEnvironment, withArguments, withStdin, withTerminal
  ) where

import Data.Text (Text)
import Tidepool.Effects.Core (CommandSpec (..), CommandInput (..), CommandSourceCapture (..))

-- Internal constructors; Tidepool.Command exposes descriptions and opaque jobs.
newtype Command = Command CommandSpec deriving (Eq, Show)
data Job = Job !Text deriving (Eq, Show)
data Memory = MiB Int | GiB Int deriving (Eq, Show)

argv :: [Text] -> Command
argv args = Command (CommandSpec args Nothing [] (256 * 1024 * 1024) ClosedInput NoCapture)

describe :: Command -> CommandSpec
describe (Command spec) = spec

-- | Capture the command's working directory, Git head, and dirty state before
-- its process starts. This costs a separate admitted command, so request it
-- only when the resulting report needs source evidence.
withSource :: Command -> Command
withSource (Command spec) = Command spec { commandSourceCapture = CaptureBeforeStart }

bashCommand :: Text -> Command
bashCommand script = argv ["bash", "--noprofile", "--norc", "-c", script, "exomonad-bash"]

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
