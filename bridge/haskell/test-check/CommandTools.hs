{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeOperators #-}

module Main (main, tests) where

import Control.Monad (unless)
import Control.Monad.Freer (Eff, interpret, run)
import qualified Control.Monad.Freer.State as State
import Data.Char (ord)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import qualified Data.Text as T
import qualified Tidepool.Aeson.FromJSON as JSON
import Tidepool.Aeson.Schema (JsonSchema (..))
import Tidepool.Aeson.Value (ToJSON (..))
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Tools
import qualified Tidepool.Effects.Core as Core
import Tidepool.Effects.Core (Commands (..))
import Tidepool.Test.Runner

data Event
  = Presented
  | LegacyWait Text
  | Awaited Text Int
  | ReadPage Text Cmd.CommandStream Cmd.CommandPosition
  | Detached Text
  deriving (Eq, Show)

newtype Service = Service [Event]

runCommands :: Eff '[Commands, State.State Service] a -> (a, [Event])
runCommands action =
  let (answer, Service events) = run (State.runState (Service []) (interpret handle action))
   in (answer, reverse events)
  where
    record event = State.modify (\(Service events) -> Service (event : events))
    handle :: Commands a -> Eff '[State.State Service] a
    handle request = case request of
      CommandStartWith _ -> pure (Right "job-immediate")
      CommandBackgroundWith _ -> pure (Right "job-background")
      CommandStatusWith _ -> pure (Right Cmd.CommandRunning)
      CommandAwaitWith key milliseconds -> do
        record (Awaited key milliseconds)
        pure $ if key == "cancel-job"
          then Left (Cmd.CommandUnavailable "await failed")
          else if milliseconds == -1 then Right finishedStatus else Right Cmd.CommandRunning
      CommandAwaitAndNotifyWith _ _ -> pure (Right Cmd.CommandRunning)
      CommandWaitWith key -> record (LegacyWait key) >> pure (Right (Core.CommandObservation (Cmd.CommandResult (Cmd.CommandExited 0) Cmd.CommandClean) (Right largeOutput)))
      CommandPresentWith _ _ -> record Presented
      CommandRetainJobWith _ -> pure (Right "commandBinding")
      CommandOutputWith key _ -> pure $ if key == "error-observe-job"
        then Left (Cmd.CommandUnavailable (T.replicate 4000 "λ"))
        else Right largeOutput
      CommandReadWith key stream position -> do
        record (ReadPage key stream position)
        pure $ if key == "error-read-job"
          then Left (Cmd.CommandUnavailable (T.replicate 4000 "λ"))
          else Right (if key == "whole-page-job" then completePage else partialFinishedPage)
      CommandInputWith _ _ -> pure (Right ())
      CommandFinishInputWith _ _ -> pure (Left (Cmd.CommandInputAcceptedCloseUnconfirmed "EOF not confirmed"))
      CommandCloseInputWith _ -> pure (Right ())
      CommandResizeWith _ _ _ -> pure (Right ())
      CommandDetachWith key -> record (Detached key) >> pure (Right ())
      CommandCancelWith _ -> pure (Right ())

finishedStatus :: Cmd.CommandStatus
finishedStatus = Cmd.CommandFinished (Cmd.CommandResult (Cmd.CommandExited 0) Cmd.CommandClean)

largeOutput :: Cmd.CommandOutput
largeOutput =
  let text = T.replicate 3000 "λ\n"
      stdout = Cmd.CommandPage text 0 (utf8Bytes text) 90000 0 0 True False False False
   in Cmd.CommandOutput stdout (page "" True)

partialFinishedPage :: Cmd.CommandPage
partialFinishedPage = Cmd.CommandPage "λ" 0 2 5 0 0 True False False False

completePage :: Cmd.CommandPage
completePage = Cmd.CommandPage "λ" 0 2 2 0 0 True False False False

page :: Text -> Bool -> Cmd.CommandPage
page text finished =
  let bytes = utf8Bytes text
   in Cmd.CommandPage text 0 bytes bytes 0 0 finished False False False

executeInput :: Maybe Int -> Maybe Bool -> Tools.Execute
executeInput wait background =
  Tools.Execute "printf λ" Nothing Nothing Nothing Nothing Nothing wait (Just 1024) Nothing Nothing background

tests :: TestTree
tests =
  let (results, events) = runCommands $ do
        immediate <- Tools.execute (executeInput Nothing Nothing)
        yielded <- Tools.execute (executeInput (Just 0) Nothing)
        background <- Tools.execute (executeInput Nothing (Just True))
        accepted <- Tools.writeInput (Tools.WriteInput "input-job" (Just "λ") (Just True) Nothing (Just 1024))
        repeated <- Tools.writeInput (Tools.WriteInput "observe-job" Nothing Nothing Nothing (Just 1024))
        output <- Tools.readRetained (Tools.ReadOutput "page-job" Nothing Nothing (Just 1024))
        fullOutput <- Tools.readRetained (Tools.ReadOutput "whole-page-job" Nothing Nothing (Just 1024))
        readError <- Tools.readRetained (Tools.ReadOutput "error-read-job" (Just Tools.Stderr) (Just 17) (Just 1024))
        observeError <- Tools.writeInput (Tools.WriteInput "error-observe-job" Nothing Nothing Nothing (Just 1024))
        cancelled <- Tools.cancelRetained (Tools.CancelCommand "cancel-job" Nothing (Just 1024))
        pure (immediate, yielded, background, accepted, repeated, output, fullOutput, readError, observeError, cancelled)
      (immediate, yielded, background, accepted, repeated, output, fullOutput, readError, observeError, cancelled) = results
      (uncroppedPrefix, _) = runCommands $
        Tools.execute (Tools.Execute "printf λ" Nothing Nothing Nothing Nothing Nothing Nothing (Just 32768) Nothing Nothing Nothing)
      noPresented = not (any isPresentation events)
      immediateUsesStatusOnlyWait = Awaited "job-immediate" (-1) `elem` events && not (any isLegacyWait events)
      yieldedKeepsTimedWait = Awaited "job-immediate" 0 `elem` events
      initialReferences =
        "session_id: job-immediate" `T.isInfixOf` Tools.presentation immediate
          && "retained as commandBinding" `T.isInfixOf` Tools.presentation immediate
          && "session_id: job-background" `T.isInfixOf` Tools.presentation background
      repeatedOmitsIntro = not ("session_id:" `T.isInfixOf` Tools.presentation repeated)
      boundedUnicode = all ((<= 1024) . utf8Bytes . Tools.presentation) [immediate, yielded, background, accepted, repeated, output, fullOutput, readError, observeError, cancelled]
      exactInputReceipt = case Tools.facts accepted of
        Tools.InputReceipt _ Tools.Acknowledged bytes Tools.UnconfirmedEof -> bytes == 2
        _ -> False
      receiptWire = case JSON.fromJSON (toJSON (Tools.facts accepted)) of
        JSON.Success (Tools.InputReceipt _ Tools.Acknowledged bytes Tools.UnconfirmedEof) -> bytes == 2
        _ -> False
      resultWireOmitsPresentation = toJSON accepted == toJSON (Tools.facts accepted)
      schemaMatches = jsonSchema (Proxy :: Proxy Tools.CommandToolResult) == jsonSchema (Proxy :: Proxy Tools.CommandToolFacts)
      pageFacts = case Tools.facts output of
        Tools.OutputPage _ Tools.OutputStdout 0 2 2 5 False 0 False 1 False -> True
        _ -> False
      completePageFacts = case Tools.facts fullOutput of
        Tools.OutputPage _ Tools.OutputStdout 0 2 2 2 True 0 False 1 True -> True
        _ -> False
      pageTextHasState =
        "bytes 0..2 of 5 · more available" `T.isInfixOf` Tools.presentation output
          && "next_offset: 2" `T.isInfixOf` Tools.presentation output
          && not ("lost 0 bytes" `T.isInfixOf` Tools.presentation output)
          && not ("lossy UTF-8" `T.isInfixOf` Tools.presentation output)
          && not ("next_offset:" `T.isInfixOf` Tools.presentation fullOutput)
      acceptedCancelDespiteAwaitError = case Tools.facts cancelled of
        Tools.Cancellation _ Tools.Accepted Nothing Nothing Nothing Nothing -> "could not be confirmed" `T.isInfixOf` Tools.presentation cancelled
        _ -> False
      readErrorKeepsRecovery = case Tools.facts readError of
        Tools.Rejected Tools.OpReadOutput (Just "error-read-job") _ Tools.NoSideEffect ->
          "[error detail shortened]" `T.isInfixOf` Tools.presentation readError
            && "read_output(session_id=\"error-read-job\", stream=\"Stderr\", offset=17)" `T.isInfixOf` Tools.presentation readError
        _ -> False
      observeErrorKeepsRecovery = case Tools.facts observeError of
        Tools.ObservedCommand {Tools.complete = False} ->
          "[error detail shortened]" `T.isInfixOf` Tools.presentation observeError
            && "read_output(session_id=\"error-observe-job\", stream=\"Stdout\", offset=0)" `T.isInfixOf` Tools.presentation observeError
        _ -> False
      largeResultIncomplete = case Tools.facts immediate of
        Tools.ObservedCommand {Tools.complete = False, Tools.payload_lines = visibleLines} ->
          visibleLines > 0
            && visibleLines < 3000
            && "stdout · bytes 0..9000 of 90000 · more available" `T.isInfixOf` Tools.presentation immediate
            && "read_output(session_id=\"job-immediate\", stream=\"Stdout\", offset=0)" `T.isInfixOf` Tools.presentation immediate
        _ -> False
      uncroppedPrefixIncomplete = case Tools.facts uncroppedPrefix of
        Tools.ObservedCommand {Tools.complete = False, Tools.payload_lines = 3000} ->
          not ("[payload clipped" `T.isInfixOf` Tools.presentation uncroppedPrefix)
            && "stream=\"Stdout\", offset=9000" `T.isInfixOf` Tools.presentation uncroppedPrefix
        _ -> False
  in testGroup "command tool receipt contract"
    [ testCase "named command routes never request host presentation" $
        check "named command routes never request host presentation" noPresented
    , testCase "immediate command blocks through status-only wait without capturing output" $
        check "immediate command blocks through status-only wait without capturing output" immediateUsesStatusOnlyWait
    , testCase "timed observation preserves its requested wait" $
        check "timed observation preserves its requested wait" yieldedKeepsTimedWait
    , testCase "new command receipts introduce their session and binding" $
        check "new command receipts introduce their session and binding" initialReferences
    , testCase "later observation omits repeated session introduction" $
        check "later observation omits repeated session introduction" repeatedOmitsIntro
    , testCase "all returned text fits max_output_bytes in UTF-8 bytes" $
        check "all returned text fits max_output_bytes in UTF-8 bytes" boundedUnicode
    , testCase "accepted input with unconfirmed EOF remains acknowledged" $
        check "accepted input with unconfirmed EOF remains acknowledged" exactInputReceipt
    , testCase "closed facts round-trip without the presentation payload" $
        check "closed facts round-trip without the presentation payload" (receiptWire && resultWireOmitsPresentation && schemaMatches)
    , testCase "finished partial read is distinct from complete stream EOF" $
        check "finished partial read is distinct from complete stream EOF" (pageFacts && pageTextHasState && completePageFacts)
    , testCase "accepted cancellation survives a failed follow-up await" $
        check "accepted cancellation survives a failed follow-up await" acceptedCancelDespiteAwaitError
    , testCase "long output-read failure retains its bounded recovery pointer" $
        check "long output-read failure retains its bounded recovery pointer" readErrorKeepsRecovery
    , testCase "long observation failure retains its bounded recovery pointer" $
        check "long observation failure retains its bounded recovery pointer" observeErrorKeepsRecovery
    , testCase "capped output from a larger finished stream remains incomplete with recovery" $
        check ("capped output from a larger finished stream remains incomplete with recovery: " <> T.unpack (Tools.presentation immediate)) largeResultIncomplete
    , testCase "fetched prefix within the display budget remains incomplete" $
        check "fetched prefix within the display budget remains incomplete" uncroppedPrefixIncomplete
    , testCase "yield route detaches a still-running job" $
        check "yield route detaches a still-running job" (Detached "job-immediate" `elem` events)
    ]
  where
    isPresentation Presented = True
    isPresentation _ = False
    isLegacyWait (LegacyWait _) = True
    isLegacyWait _ = False

check :: String -> Bool -> IO ()
check label condition = unless condition (ioError (userError ("failed: " <> label)))

utf8Bytes :: Text -> Int
utf8Bytes = T.foldl' (\bytes character -> bytes + width character) 0
  where
    width character
      | ord character < 0x80 = 1
      | ord character < 0x800 = 2
      | ord character < 0x10000 = 3
      | otherwise = 4

main :: IO ()
main = runTests tests
