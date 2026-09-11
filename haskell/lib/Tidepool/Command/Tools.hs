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
    offset :: Maybe Int
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
          "Read retained output without executing, waiting or consuming it. Default stream Stdout, offset 0. Reply reports byte positions, next offset, current end versus EOF and retention gaps. Use Stderr for diagnostics."
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
        Left (Cmd.CommandInputAcceptedCloseUnconfirmed detail) ->
          pure $ "session_id: " <> key <> "\nBackend acknowledged the write; child consumption is unknown. EOF unconfirmed: " <> detail <> "\nRetry close-only with write_stdin(close_stdin=true), without chars. Do not resend these bytes."
        Left issue -> pure $ "session_id: " <> key <> "\nInput operation failed or was unconfirmed: " <> T.pack (show issue) <> "\nInspect the same job before recovery. Do not replay input after an uncertain acknowledgment."
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
readRetained ReadOutput {session_id = key, stream = selected, offset = position} = do
  let selectedStream = case selected of
        Just Stderr -> Cmd.Stderr
        _ -> Cmd.Stdout
  result <- send (CommandReadWith key selectedStream (Cmd.OutputOffset (fromMaybe 0 position)))
  pure $ case result of
    Left Cmd.CommandOutputPending -> "session_id: " <> key <> "\nNo output yet; streams are starting."
    Left issue -> "session_id: " <> key <> "\nOutput unavailable: " <> T.pack (show issue) <> "\nInspect the same job; do not rerun the command."
    Right details -> T.pack (show selectedStream) <> " · " <> renderPage details

renderPage :: Cmd.CommandPage -> Text
renderPage details =
  let prefix = T.take 6000 (Cmd.outputText details)
      shortened = T.length prefix < T.length (Cmd.outputText details)
      byteCount = T.foldl' (\n c -> n + utf8Width c) 0 prefix
      nextOffset = Cmd.outputStart details + byteCount
      visible =
        details
          { Cmd.outputText = prefix,
            Cmd.outputEnd = nextOffset,
            Cmd.outputTrailingFragment = shortened && not (T.isSuffixOf "\n" prefix)
          }
      (text, _) = displayWith 28000 (if shortened && not (Cmd.outputLossy details) then visible else details)
  in if shortened && Cmd.outputLossy details
      then "Lossy UTF-8; displayed text is abbreviated. Exact byte positions refer to the retained page, not the displayed prefix. Supply offset to inspect another range.\n" <> text
      else text <> "\nnext_offset: " <> T.pack (show (if shortened then nextOffset else Cmd.outputEnd details))
  where
    utf8Width c
      | ord c < 0x80 = 1
      | ord c < 0x800 = 2
      | ord c < 0x10000 = 3
      | otherwise = 4
