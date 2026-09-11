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
    CancelCommand (..),
    Stream (..),
    tools,
    execute,
    writeInput,
    readRetained,
    cancelRetained,
  )
where

import Control.Monad.Freer (Eff, Member, send)
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
import Tidepool.Effects.Core (Commands (..))
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
    close_stdin :: Maybe Bool,
    yield_time_ms :: Maybe Int,
    max_output_bytes :: Maybe Int
  }
  deriving (Generic, FromJSON, JsonSchema)

data CancelCommand = CancelCommand
  { session_id :: Text,
    yield_time_ms :: Maybe Int,
    max_output_bytes :: Maybe Int
  }
  deriving (Generic, FromJSON, JsonSchema)

data Stream = Stdout | Stderr
  deriving (Generic, FromJSON, JsonSchema)

data ReadOutput = ReadOutput
  { session_id :: Text,
    stream :: Maybe Stream,
    offset :: Maybe Int,
    max_output_bytes :: Maybe Int
  }
  deriving (Generic, FromJSON, JsonSchema)

-- Schemas and handlers travel together through the ordinary tools-record DSL.
-- These definitions can be selected or reused by project-authored tool records.
data ShellTools mode = ShellTools
  { bash :: mode :- RawCall Text,
    execCommand :: mode :- Call Execute Text,
    writeStdin :: mode :- Call WriteInput Text,
    readOutput :: mode :- Call ReadOutput Text,
    cancelCommand :: mode :- Call CancelCommand Text
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
          "Send chars to an existing session_id, then observe new output. Omit chars or use empty text to poll without writing. close_stdin sends final bytes then EOF for pipes only; PTYs reject it. Backend write acknowledgment does not prove the child consumed the bytes. yield_time_ms: 0..30000 (default 250). Output allowance: 1024..32768 bytes. PTYs accept terminal control characters. Never rerun to recover output."
          writeInput,
      readOutput =
        tool
          "Read retained output without executing, waiting or consuming it. Default stream Stdout, offset 0, max_output_bytes 8192 (1024..32768 including metadata). Contiguous pages, never head/tail previews. Reply reports byte positions, next offset, current end versus EOF and retention gaps. Use Stderr for diagnostics."
          readRetained,
      cancelCommand =
        tool
          "Request cancellation of an existing session_id, then observe it. Works for queued jobs, pipes and PTYs; stdin need not be open. Default observation 250ms, maximum 30000ms. Cancellation requested is not terminal/cleanup confirmation. Repeated cancellation preserves the actual outcome; retained output remains readable."
          cancelRetained
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
writeInput WriteInput {session_id = key, chars = input, close_stdin = close, yield_time_ms = wait, max_output_bytes = limit} =
  case observation 250 wait limit of
    options@Cmd.Observation {} -> do
      let text = fromMaybe "" input
          eof = fromMaybe False close
      receipt <- case (T.null text, eof) of
        (True, False) -> pure (Right ())
        (True, True) -> send (CommandCloseInputWith key)
        (False, False) -> send (CommandInputWith key text)
        (False, True) -> send (CommandFinishInputWith key text)
      case receipt of
        Left (Cmd.CommandInputRejected detail) ->
          pure $ "session_id: " <> key <> "\nRejected · input not submitted (including chars); EOF not submitted · " <> detail
        Left Cmd.CommandUnauthorized ->
          pure $ "session_id: " <> key <> "\nRejected · input not submitted (including chars); EOF not submitted · input control is not authorized"
        Left (Cmd.CommandInputAcceptedCloseUnconfirmed detail) ->
          pure $ "session_id: " <> key <> "\nBackend acknowledged the write; child consumption is unknown. EOF unconfirmed: " <> detail <> "\nRetry close-only with write_stdin(close_stdin=true), without chars. Do not resend these bytes."
        Left issue -> pure $ "session_id: " <> key <> "\nInput submission unconfirmed: " <> T.pack (show issue) <> "\nInspect the same job before recovery. Do not replay input after an uncertain acknowledgment."
        Right () -> do
          _ <- Cmd.observe options (Job key)
          pure $ if eof then "Stdin is closed." else ""

cancelRetained :: (Member Cmd.Commands effects) => CancelCommand -> Eff effects Text
cancelRetained CancelCommand {session_id = key, yield_time_ms = wait, max_output_bytes = limit} =
  case observation 250 wait limit of
    options@Cmd.Observation {} -> do
      receipt <- send (CommandCancelWith key)
      case receipt of
        Left issue -> pure $ "session_id: " <> key <> "\nCancellation unconfirmed: " <> T.pack (show issue) <> "\nInspect the same job; do not start a replacement."
        Right () -> do
          current <- Cmd.observe options (Job key)
          pure $ case current of
            Cmd.CommandFinished _ -> ""
            _ -> "Cancellation requested; terminal outcome and cleanup are not yet confirmed."

readRetained :: (Member Cmd.Commands effects) => ReadOutput -> Eff effects Text
readRetained ReadOutput {session_id = key, stream = selected, offset = position, max_output_bytes = limit} =
  case observation 0 (Just 0) (Just (fromMaybe 8192 limit)) of
    Cmd.Observation {outputBytes = budget} -> do
      let selectedStream = case selected of
            Just Stderr -> Cmd.Stderr
            _ -> Cmd.Stdout
          offset = fromMaybe 0 position
          contentBudget = budget - 512
          read bytes = send (CommandReadWith key selectedStream (Cmd.OutputSlice offset bytes))
      first <- read contentBudget
      -- Replacement characters can expand invalid UTF-8. Ask the byte owner for
      -- a smaller page rather than inventing offsets from decoded text.
      result <- case first of
        Right page | utf8Bytes (Cmd.outputText page) > contentBudget -> read (max 1 (contentBudget `div` 3))
        _ -> pure first
      pure $ case result of
        Left Cmd.CommandOutputPending -> "session_id: " <> key <> "\nNo output yet; streams are starting."
        Left issue -> "session_id: " <> key <> "\nOutput unavailable: " <> T.pack (show issue) <> "\nInspect the same job; do not rerun the command."
        Right details ->
          let (text, _) = displayWith budget details
          in T.pack (show selectedStream) <> " · " <> text <> "\nnext_offset: " <> T.pack (show (Cmd.outputEnd details))

utf8Bytes :: Text -> Int
utf8Bytes = T.foldl' (\n c -> n + width c) 0
  where
    width c
      | ord c < 0x80 = 1
      | ord c < 0x800 = 2
      | ord c < 0x10000 = 3
      | otherwise = 4
