{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeOperators #-}

module Main (main) where

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

data Event
  = Presented
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
      CommandAwaitWith key _
        | key == "cancel-job" -> pure (Left (Cmd.CommandUnavailable "await failed"))
        | otherwise -> pure (Right Cmd.CommandRunning)
      CommandAwaitAndNotifyWith _ _ -> pure (Right Cmd.CommandRunning)
      CommandWaitWith _ -> pure (Right (Core.CommandObservation (Cmd.CommandResult (Cmd.CommandExited 0) Cmd.CommandClean) (Right largeOutput)))
      CommandPresentWith _ _ -> record Presented
      CommandRetainJobWith _ -> pure (Right "commandBinding")
      CommandOutputWith _ _ -> pure (Right largeOutput)
      CommandReadWith key stream position -> do
        record (ReadPage key stream position)
        pure (Right (if key == "whole-page-job" then completePage else partialFinishedPage))
      CommandInputWith _ _ -> pure (Right ())
      CommandFinishInputWith _ _ -> pure (Left (Cmd.CommandInputAcceptedCloseUnconfirmed "EOF not confirmed"))
      CommandCloseInputWith _ -> pure (Right ())
      CommandResizeWith _ _ _ -> pure (Right ())
      CommandDetachWith key -> record (Detached key) >> pure (Right ())
      CommandCancelWith _ -> pure (Right ())

largeOutput :: Cmd.CommandOutput
largeOutput = Cmd.CommandOutput (page (T.replicate 3000 "λ") True) (page "" True)

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

main :: IO ()
main = do
  let (results, events) = runCommands $ do
        immediate <- Tools.execute (executeInput Nothing Nothing)
        yielded <- Tools.execute (executeInput (Just 0) Nothing)
        background <- Tools.execute (executeInput Nothing (Just True))
        accepted <- Tools.writeInput (Tools.WriteInput "input-job" (Just "λ") (Just True) Nothing (Just 1024))
        repeated <- Tools.writeInput (Tools.WriteInput "observe-job" Nothing Nothing Nothing (Just 1024))
        output <- Tools.readRetained (Tools.ReadOutput "page-job" Nothing Nothing (Just 1024))
        fullOutput <- Tools.readRetained (Tools.ReadOutput "whole-page-job" Nothing Nothing (Just 1024))
        cancelled <- Tools.cancelRetained (Tools.CancelCommand "cancel-job" Nothing (Just 1024))
        pure (immediate, yielded, background, accepted, repeated, output, fullOutput, cancelled)
      (immediate, yielded, background, accepted, repeated, output, fullOutput, cancelled) = results
      noPresented = not (any isPresentation events)
      initialReferences =
        "session_id: job-immediate" `T.isInfixOf` Tools.presentation immediate
          && "retained as commandBinding" `T.isInfixOf` Tools.presentation immediate
          && "session_id: job-background" `T.isInfixOf` Tools.presentation background
      repeatedOmitsIntro = not ("session_id:" `T.isInfixOf` Tools.presentation repeated)
      boundedUnicode = all ((<= 1024) . utf8Bytes . Tools.presentation) [immediate, yielded, background, accepted, repeated, output, fullOutput, cancelled]
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
      pageTextHasState = all (`T.isInfixOf` Tools.presentation output) ["available_end=5", "eof=false", "lost_bytes=0", "lossy_utf8=false"]
      acceptedCancelDespiteAwaitError = case Tools.facts cancelled of
        Tools.Cancellation _ Tools.Accepted Nothing Nothing Nothing Nothing -> "could not be confirmed" `T.isInfixOf` Tools.presentation cancelled
        _ -> False
      largeResultIncomplete = case Tools.facts immediate of
        Tools.ObservedCommand {Tools.complete = False} -> True
        _ -> False
  check "named command routes never request host presentation" noPresented
  check "new command receipts introduce their session and binding" initialReferences
  check "later observation omits repeated session introduction" repeatedOmitsIntro
  check "all returned text fits max_output_bytes in UTF-8 bytes" boundedUnicode
  check "accepted input with unconfirmed EOF remains acknowledged" exactInputReceipt
  check "closed facts round-trip without the presentation payload" (receiptWire && resultWireOmitsPresentation && schemaMatches)
  check "finished partial read is distinct from complete stream EOF" (pageFacts && pageTextHasState && completePageFacts)
  check "accepted cancellation survives a failed follow-up await" acceptedCancelDespiteAwaitError
  check "output beyond the presentation budget is marked incomplete" largeResultIncomplete
  check "yield route detaches a still-running job" (Detached "job-immediate" `elem` events)
  putStrLn "passed: command tool receipt behavior (10 checks)"
  where
    isPresentation Presented = True
    isPresentation _ = False

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
