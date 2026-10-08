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
module FormJevDialogueTest (formJevDialogueTests) where
import Prelude
import Data.Text (Text)
import qualified Data.Text as T
import Control.Monad.Freer (Eff, Member, interpret, run)
import Control.Monad.Freer.State (State, get, modify, runState)
import Test.Tasty.HUnit (assertEqual)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)
import Tidepool.Aeson.Value
import Tidepool.Effects.Core
  (AskUser(..), Jev(..), JevCallError(..), Console(..), FormLease(..), FormAttemptId(..), FormAttempt(..), FormTransition(..))
import qualified Jev.Operators as J

import qualified Examples.JevFormWorkflow as Workflow

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
runDialogue :: [FormAttempt] -> (Workflow.DialogueResult '[AskUser,Jev,Console,State Script],Script)
runDialogue replies = run (runState (initial replies) (interpret consoleHost (interpret jevHost (interpret formHost Workflow.dialogue))))
submitted :: Text -> Text -> FormAttempt
submitted token value = FormSubmitted (FormAttemptToken token) (object ["f0" .= value])

formJevDialogueTests :: TestTree
formJevDialogueTests = testGroup "form-jev-dialogue"
  [ testCase "typed Jev failure selects original human continuation and appends next form" fallbackContinues
  , testCase "dismissed human fallback runs neither continuation nor another inference" fallbackDismissed
  , testCase "missing criteria chosen by a human retains its separate domain result" fallbackMissingCriteria
  , testCase "dismissed follow-up retains its chosen origin and meaning" followupDismissed
  ]
fallbackContinues :: IO ()
fallbackContinues = do
  let (outcome,final) = runDialogue [submitted "subject" "Inspect the change",submitted "fallback" "o0",submitted "followup" "Keep the original action"]
  case Workflow.outcome outcome of
    Workflow.Finished (Workflow.HumanAfterInferenceFailure (J.Transport (JevCircuitOpen 503 987)))
      (Workflow.Delivery Workflow.Express)
      (Workflow.DeliveryPrepared Workflow.Express "Inspect the change" "Keep the original action") -> pure ()
    actual -> error ("typed failure or selected original value lost: " ++ show actual)
  assertEqual "exact effect history includes only chosen continuation"
    [Opened,Awaited,Committed,Inferred,Opened,Awaited,Committed,Opened,Awaited,Committed]
    [event | event <- events final, case event of Displayed _ -> False; _ -> True]
  assertEqual "only authored final presentation executes Console" 1
    (length [() | Displayed _ <- events final])
  assertEqual "all mounted leases settled" [] (leases final)
  assertEqual "all supplied answers consumed" [] (answers final)
  case Workflow.retainedResponse outcome of
    Nothing -> pure ()
    Just _ -> error "transport failure acquired a response"
fallbackDismissed :: IO ()
fallbackDismissed = do
  let (outcome,final) = runDialogue [submitted "subject" "Inspect the change",FormDismissed]
  case Workflow.outcome outcome of
    Workflow.SelectionDismissed (Workflow.HumanAfterInferenceFailure (J.Transport (JevCircuitOpen 503 987))) -> pure ()
    actual -> error (show actual)
  assertEqual "dismissal ends before selected action or another inference" [Opened,Awaited,Committed,Inferred,Opened,Awaited,Closed] (events final)
  assertEqual "dismissed lease closed" [] (leases final)

fallbackMissingCriteria :: IO ()
fallbackMissingCriteria = do
  let (result,final) = runDialogue
        [submitted "subject" "A delivery",submitted "fallback" "o2",submitted "criteria" "Tomorrow, price 8"]
  case Workflow.outcome result of
    Workflow.Finished (Workflow.HumanAfterInferenceFailure (J.Transport (JevCircuitOpen 503 987)))
      Workflow.MissingDeliveryCriteria (Workflow.CriteriaSupplied "A delivery" "Tomorrow, price 8") -> pure ()
    actual -> error (show actual)
  assertEqual "one inference and one selected follow-up" 1 (length [() | Inferred <- events final])
  assertEqual "three independent sequential forms" 3 (length [() | Opened <- events final])
  assertEqual "clarification leaves no live leases" [] (leases final)

followupDismissed :: IO ()
followupDismissed = do
  let (result,final) = runDialogue [submitted "subject" "A delivery",submitted "fallback" "o1",FormDismissed]
  case Workflow.outcome result of
    Workflow.ContinuationDismissed (Workflow.HumanAfterInferenceFailure (J.Transport (JevCircuitOpen 503 987)))
      (Workflow.Delivery Workflow.Economy) -> pure ()
    actual -> error (show actual)
  assertEqual "the selected follow-up closes once" 1 (length [() | Closed <- events final])
  assertEqual "dismissal does not display a final result" 0 (length [() | Displayed _ <- events final])
  assertEqual "dismissal releases all form leases" [] (leases final)
