{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
-- | A notebook-style dialogue through the real Form and Jev public surfaces.
-- Host GHC executes typed transport failure/fallback. The successful response
-- fixture requires Tidepool's native JSON anchors and is a separate native gate.
-- form-choice-response.json was emitted by Jev's test/Proto.stub "quick"
-- with Core.prepare and official Aeson at core revision 2883fdc38cc7a64572e76ea43bd38e1df3a5e28b.
module FormJevDialogueTest (formJevDialogueTests, dialogue) where
import Prelude
import Data.Text (Text)
import qualified Data.Text as T
import Data.List.NonEmpty (NonEmpty(..))
import Control.Monad.Freer (Eff, Member, interpret, run)
import Control.Monad.Freer.State (State, get, modify, runState)
import Test.Tasty.HUnit (assertEqual)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)
import Tidepool.Aeson.Value
import qualified Tidepool.Form as F
import qualified Tidepool.View as V
import Tidepool.Inspection (display)
import Tidepool.Effects.Core
  (AskUser(..), Jev(..), JevCallError(..), Console(..), FormLease(..), FormAttemptId(..), FormAttempt(..), FormTransition(..))
import qualified Jev.Operators as J

data SelectionFailure = InferenceFailure (J.JevError JevCallError) | PolicyDoubt J.Doubt
  deriving (Show)
data DialogueResult = Finished (Maybe SelectionFailure) Text Text
  | FirstDismissed | FallbackDismissed SelectionFailure | FollowupDismissed
  | FormFailed F.FormCause deriving (Show)

-- The same original action pairs supply both the semantic offers and the
-- human fallback. Preparing or displaying the unselected action must be lazy.
dialogue :: (Member AskUser effects, Member Jev effects, Member Console effects) => Eff effects DialogueResult
dialogue = do
  first <- F.askUser (F.textInput "Subject" Nothing)
  case first of
    F.Submitted subject -> do
      let quick = (V.text "Quick path", F.askUser (F.textInput "Follow-up" Nothing))
          careful = (V.text "Careful path", error "unselected original continuation was demanded")
          packet = #route J.:= J.choice "Which path?"
            (J.alt #quick "Quick path" (snd quick) J..| J.alt #careful "Careful path" (snd careful))
          world = J.rawState (object ["subject" .= subject])
          runSelected failure action = do
            followup <- action
            case followup of
              F.Submitted reason -> do
                _ <- display (V.column [V.text subject, V.text reason])
                pure (Finished failure subject reason)
              F.Dismissed -> pure FollowupDismissed
              F.FormUnavailable cause -> pure (FormFailed cause)
          humanFallback failure = do
            selected <- F.askUser (F.choice "Next step"
              (F.option (fst quick) (snd quick) :| [F.option (fst careful) (snd careful)]))
            case selected of
              F.Submitted action -> runSelected (Just failure) action
              F.Dismissed -> pure (FallbackDismissed failure)
              F.FormUnavailable cause -> pure (FormFailed cause)
      inferred <- J.ask world packet
      case inferred of
        Left failure -> humanFallback (InferenceFailure failure)
        Right response -> case J.takenUnder J.careful (J.answers response).route of
          Left doubt -> humanFallback (PolicyDoubt doubt)
          Right selected -> runSelected Nothing (J.settledValue selected)
    F.Dismissed -> pure FirstDismissed
    F.FormUnavailable cause -> pure (FormFailed cause)

data Event = Opened | Awaited | Committed | Closed | Inferred | Displayed Value deriving (Eq,Show)
data Script = Script { answers :: [FormAttempt], events :: [Event], nextMount :: Int, leases :: [FormLease], currentAttempt :: Maybe FormAttemptId }
initial :: [FormAttempt] -> Script
initial replies = Script replies [] 0 [] Nothing
mark :: Event -> Eff '[State Script] ()
mark event = modify (\s -> s {events=events s ++ [event]})
formHost :: AskUser a -> Eff '[Jev,Console,State Script] a
formHost (FormOpenWith _) = do
  state <- get
  let lease = FormLeaseToken ("mount-" <> T.pack (show (nextMount state)))
  modify (\s -> s {nextMount=nextMount s+1,leases=lease:leases s,events=events s ++ [Opened]})
  pure (Right lease)
formHost (FormAwaitWith lease) = do
  requireLease lease
  state <- get
  case answers state of
    reply:rest -> do
      let attempt = case reply of FormSubmitted key _ -> Just key; FormDismissed -> Nothing
      modify (\s -> s {answers=rest,currentAttempt=attempt,events=events s ++ [Awaited]})
      pure (Right reply)
    [] -> error "unexpected additional human question"
formHost (FormCommitWith lease attempt _) = do
  requireLease lease
  current <- currentAttempt <$> get
  if current /= Just attempt then error "wrong submitted attempt committed" else pure ()
  modify (\s -> s {leases=filter (/=lease) (leases s),events=events s ++ [Committed]})
  pure (Right FormApplied)
formHost (FormRejectWith _ _ _) = error "valid scripted answer unexpectedly rejected"
formHost (FormCloseWith lease) = do
  modify (\s -> s {leases=filter (/=lease) (leases s),events=events s ++ [Closed]})
  pure (Right ())
requireLease :: Member (State Script) effects => FormLease -> Eff effects ()
requireLease lease = do
  active <- leases <$> get
  if lease `elem` active then pure () else error "operation used a settled or unknown mount"
jevHost :: Jev a -> Eff '[Console,State Script] a
jevHost (JevAskWith _) = do
  modify (\s -> s {events=events s ++ [Inferred]})
  -- The real generated error bypasses JSON response decoding. We do not
  -- replace Tidepool's native encode/decode anchors to fabricate host success.
  pure (Left (JevCircuitOpen 503 987))
consoleHost :: Console a -> Eff '[State Script] a
consoleHost DisplayAllowanceWith = pure 8192
consoleHost (DisplayViewWith view) = mark (Displayed view)
consoleHost _ = error "dialogue used an unexpected console operation"
runDialogue :: [FormAttempt] -> (DialogueResult,Script)
runDialogue replies = run (runState (initial replies) (interpret consoleHost (interpret jevHost (interpret formHost dialogue))))
submitted :: Text -> Text -> FormAttempt
submitted token value = FormSubmitted (FormAttemptToken token) (object ["f0" .= value])

formJevDialogueTests :: TestTree
formJevDialogueTests = testGroup "form-jev-dialogue"
  [ testCase "typed Jev failure selects original human continuation and appends next form" fallbackContinues
  , testCase "dismissed human fallback runs neither continuation nor another inference" fallbackDismissed
  ]
fallbackContinues :: IO ()
fallbackContinues = do
  let (outcome,final) = runDialogue [submitted "subject" "Inspect the change",submitted "fallback" "o0",submitted "followup" "Keep the original action"]
  case outcome of
    Finished (Just (InferenceFailure (J.Transport (JevCircuitOpen 503 987)))) "Inspect the change" "Keep the original action" -> pure ()
    _ -> error ("typed failure or selected original value lost: " ++ show outcome)
  let expectedView = object ["kind" .= ("column"::Text),"children" .=
        [object ["kind" .= ("text"::Text),"text" .= ("Inspect the change"::Text),"truncated" .= False],
         object ["kind" .= ("text"::Text),"text" .= ("Keep the original action"::Text),"truncated" .= False]]]
  assertEqual "one inference, same sequential interpreter, final rich output" [Opened,Awaited,Committed,Inferred,Opened,Awaited,Committed,Opened,Awaited,Committed,Displayed expectedView] (events final)
  assertEqual "all mounted leases settled" [] (leases final)
  assertEqual "all supplied answers consumed" [] (answers final)
fallbackDismissed :: IO ()
fallbackDismissed = do
  let (outcome,final) = runDialogue [submitted "subject" "Inspect the change",FormDismissed]
  case outcome of FallbackDismissed (InferenceFailure (J.Transport (JevCircuitOpen 503 987))) -> pure (); _ -> error (show outcome)
  assertEqual "dismissal ends before selected action or another inference" [Opened,Awaited,Committed,Inferred,Opened,Awaited,Closed] (events final)
  assertEqual "dismissed lease closed" [] (leases final)
