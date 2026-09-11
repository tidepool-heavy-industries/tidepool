{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

module Tidepool.Command.Tools
  ( ShellTools (..),
    Execute (..),
    WriteInput (..),
    ReadOutput (..),
    Stream (..),
    tools,
    execute,
    writeInput,
    readRetained,
  )
where

import Control.Monad.Freer (Eff, Member)
import Data.Char (ord)
import qualified Data.Map.Strict as Map
import Data.Maybe (fromMaybe)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import Tidepool.Command.Types (Job (..))
import Tidepool.Inspection (Display (..))

data Execute = Execute
  { cmd :: Text,
    workdir :: Maybe Text,
    environment :: Maybe (Map.Map Text Text),
    memory_mib :: Maybe Int,
    tty :: Maybe Bool,
    stdin :: Maybe Bool,
    yield_time_ms :: Maybe Int,
    max_output_bytes :: Maybe Int
  }
  deriving (Generic, FromJSON, JsonSchema)

data WriteInput = WriteInput
  { session_id :: Text,
    chars :: Maybe Text,
    yield_time_ms :: Maybe Int,
    max_output_bytes :: Maybe Int
  }
  deriving (Generic, FromJSON, JsonSchema)

data Stream = Stdout | Stderr
  deriving (Generic, FromJSON, JsonSchema)

data ReadOutput = ReadOutput
  { session_id :: Text,
    stream :: Maybe Stream,
    offset :: Maybe Int
  }
  deriving (Generic, FromJSON, JsonSchema)

-- Schemas and handlers travel together through the ordinary tools-record DSL.
-- These definitions can be selected or reused by project-authored tool records.
data ShellTools mode = ShellTools
  { bash :: mode :- RawCall Text,
    execCommand :: mode :- Call Execute Text,
    writeStdin :: mode :- Call WriteInput Text,
    readOutput :: mode :- Call ReadOutput Text
  }
  deriving (Generic)

tools :: (Member Cmd.Commands effects) => ShellTools (AsServerT (Eff effects))
tools =
  ShellTools
    { bash =
        rawTool
          "Run literal Bash in this actor's workspace; output displays directly. Default: 256 MiB, 30s observation. Use exec_command for resource/cwd/environment/PTY options. A live command returns session_id for write_stdin; read_output recovers retained output without rerunning. Haskell retains the same Cmd.Job for composition."
          (\script -> Cmd.run (Cmd.bashCommand script) >> pure ""),
      execCommand =
        tool
          "Execute literal Bash once. Optional workdir/environment, memory_mib (default 256), tty or piped stdin. yield_time_ms: 0..30000 (default 30000); max_output_bytes: 1024..32768 (default 32768). Running/queued commands retain session_id; observation expiry does not cancel execution. Bash does not load shell profiles."
          execute,
      writeStdin =
        tool
          "Send chars to an existing session_id, then observe new output. Omit chars or use empty text to poll without writing. yield_time_ms: 0..30000 (default 250). Output allowance: 1024..32768 bytes. PTYs accept terminal control characters. Never rerun to recover output."
          writeInput,
      readOutput =
        tool
          "Read retained output without executing, waiting or consuming it. Default stream Stdout, offset 0. Reply reports byte positions, next offset, current end versus EOF and retention gaps. Use Stderr for diagnostics."
          readRetained
    }

-- Validate observation options before starting a process or sending input.
observation :: Int -> Maybe Int -> Maybe Int -> Cmd.Observation
observation defaultWait wait limit
  | milliseconds < 0 || milliseconds > 30000 = error "yield_time_ms must be 0..30000"
  | bytes < 1024 || bytes > 32768 = error "max_output_bytes must be 1024..32768"
  | otherwise = Cmd.Observation milliseconds bytes
  where
    milliseconds = fromMaybe defaultWait wait
    bytes = fromMaybe 32768 limit

execute :: (Member Cmd.Commands effects) => Execute -> Eff effects Text
execute
  Execute
    { cmd = script,
      workdir = directory,
      environment = env,
      memory_mib = memory,
      tty = terminal,
      stdin = pipe,
      yield_time_ms = wait,
      max_output_bytes = limit
    } =
    case observation 30000 wait limit of
      options@Cmd.Observation {} -> do
        let command =
              maybe id Cmd.inDirectory directory $
                Cmd.withEnvironment (maybe [] Map.toList env) $
                  Cmd.withMemory (Cmd.MiB (fromMaybe 256 memory)) $
                    input (Cmd.bashCommand script)
            input =
              if fromMaybe False terminal
                then Cmd.withTerminal
                else if fromMaybe False pipe then Cmd.withStdin else id
        retained <- Cmd.start command
        _ <- Cmd.observe options retained
        pure ""

writeInput :: (Member Cmd.Commands effects) => WriteInput -> Eff effects Text
writeInput WriteInput {session_id = key, chars = input, yield_time_ms = wait, max_output_bytes = limit} =
  case observation 250 wait limit of
    options@Cmd.Observation {} -> do
      let retained = Job key
      case input of
        Nothing -> pure ()
        Just "" -> pure ()
        Just text -> Cmd.sendInput retained text
      _ <- Cmd.observe options retained
      pure ""

readRetained :: (Member Cmd.Commands effects) => ReadOutput -> Eff effects Text
readRetained ReadOutput {session_id = key, stream = selected, offset = position} = do
  let selectedStream = case selected of
        Just Stderr -> Cmd.Stderr
        _ -> Cmd.Stdout
  page <- Cmd.readPage (Job key) selectedStream (Cmd.OutputOffset (fromMaybe 0 position))
  let details = Cmd.pageDetails page
      prefix = T.take 6000 (Cmd.pageText page)
      shortened = T.length prefix < T.length (Cmd.pageText page)
      byteCount = T.foldl' (\n c -> n + utf8Width c) 0 prefix
      nextOffset = Cmd.outputStart details + byteCount
      visible =
        details
          { Cmd.outputText = prefix,
            Cmd.outputEnd = nextOffset,
            Cmd.outputTrailingFragment = shortened && not (T.isSuffixOf "\n" prefix)
          }
      (text, _) = displayWith 28000 (if shortened && not (Cmd.outputLossy details) then visible else details)
  pure $
    if shortened && Cmd.outputLossy details
      then "Lossy UTF-8; displayed text is abbreviated. Exact byte positions refer to the retained page, not the displayed prefix. Supply offset to inspect another range.\n" <> text
      else text <> "\nnext_offset: " <> T.pack (show (if shortened then nextOffset else Cmd.outputEnd details))
  where
    utf8Width c
      | ord c < 0x80 = 1
      | ord c < 0x800 = 2
      | ord c < 0x10000 = 3
      | otherwise = 4
