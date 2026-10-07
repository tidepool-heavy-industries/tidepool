{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
-- The real generated AskUser constructors are interpreted by a scripted host.
-- This checks the Haskell continuation; native lease histories have their own owner.
module FormLifecycleTest (formLifecycleTests) where
import Prelude
import Data.Text (Text)
import Data.List.NonEmpty (NonEmpty(..))
import Control.Monad.Freer (Eff, interpret, run)
import Control.Monad.Freer.State (State, get, modify, runState)
import Test.Tasty.HUnit (assertEqual)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)
import Tidepool.Aeson.Value
import Tidepool.Form
import Tidepool.View (text)
import Tidepool.Effects.Core
  (AskUser(..), FormLease(..), FormAttemptId(..), FormAttempt(..), FormTransition(..))

data Event = Opened | Awaited | Rejected | Committed | Closed deriving (Eq,Show)
data Script = Script
  { attempts :: [Either FormCause FormAttempt]
  , commits :: [Either FormCause FormTransition]
  , closeResult :: Either FormCause ()
  , events :: [Event]
  }
script :: [Either FormCause FormAttempt] -> [Either FormCause FormTransition] -> Script
script as cs = Script as cs (Right ()) []

handle :: AskUser a -> Eff '[State Script] a
handle (FormOpenWith _) = mark Opened >> pure (Right (FormLeaseToken "same-mount"))
handle (FormAwaitWith (FormLeaseToken lease)) = do
  requireLease lease
  mark Awaited
  state <- get
  case attempts state of
    next:rest -> modify (\s -> s {attempts=rest}) >> pure next
    [] -> error "form awaited after scripted settlement"
handle (FormRejectWith (FormLeaseToken lease) (FormAttemptToken _) errors) = do
  requireLease lease
  case errors of Array (_:_) -> pure (); _ -> error "rejected form without validation errors"
  mark Rejected
  pure (Right FormApplied)
handle (FormCommitWith (FormLeaseToken lease) (FormAttemptToken _) _) = do
  requireLease lease
  mark Committed
  state <- get
  case commits state of
    next:rest -> modify (\s -> s {commits=rest}) >> pure next
    [] -> error "form committed after scripted settlement"
handle (FormCloseWith (FormLeaseToken lease)) = do
  requireLease lease
  mark Closed
  closeResult <$> get
mark :: Event -> Eff '[State Script] ()
mark event = modify (\s -> s {events=events s ++ [event]})
requireLease :: Text -> Eff '[State Script] ()
requireLease "same-mount" = pure ()
requireLease _ = error "form retried on another mount"
runForm :: Script -> Form a -> (FormResult a,[Event])
runForm initial form = let (answer,final) = run (runState initial (interpret handle (askUser form))) in (answer,events final)
submitted :: Text -> Value -> Either FormCause FormAttempt
submitted key value = Right (FormSubmitted (FormAttemptToken key) (object ["f0" .= value]))

formLifecycleTests :: TestTree
formLifecycleTests = testGroup "human-form-lifecycle"
  [ testCase "invalid corrected submission reuses one mount" invalidCorrected
  , testCase "stale commit awaits again and returns the new original closure" staleCommit
  , testCase "dismissal survives a cleanup failure" dismissal
  , testCase "transport cause survives a cleanup failure" unavailable
  , testCase "durable commit returns original value without another host operation" committedValue
  ]
invalidCorrected :: IO ()
invalidCorrected = do
  let form = validate (\n -> ["positive required" | n <= 0]) (intInput "Count" Nothing)
      (answer,seen) = runForm (script [submitted "first" (String "0"),submitted "second" (String "2")] [Right FormApplied]) form
  case answer of Submitted 2 -> pure (); _ -> error "corrected value did not survive"
  assertEqual "one open, reject, then exact commit" [Opened,Awaited,Rejected,Awaited,Committed] seen
staleCommit :: IO ()
staleCommit = do
  let form = choice "Action" (option (text "same") (+1) :| [option (text "same") (*2)])
      (answer,seen) = runForm (script [submitted "old" (String "o0"),submitted "new" (String "o1")] [Right FormStale,Right FormApplied]) form
  case answer of Submitted action -> assertEqual "latest retained function" (20::Int) (action 10); _ -> error "new selection unavailable"
  assertEqual "stale attempt does not remount" [Opened,Awaited,Committed,Awaited,Committed] seen
dismissal :: IO ()
dismissal = do
  let initial = (script [Right FormDismissed] []) {closeResult=Left (FormCleanupUnconfirmed "cleanup transport")}
      (answer,seen) = runForm initial (pure ())
  case answer of Dismissed -> pure (); _ -> error "cleanup replaced dismissal"
  assertEqual "lease closed on dismissal" [Opened,Awaited,Closed] seen
unavailable :: IO ()
unavailable = do
  let initial = (script [Left (FormTransportFailed "connection closed")] []) {closeResult=Left (FormCleanupUnconfirmed "cleanup transport")}
      (answer,seen) = runForm initial (pure ())
  case answer of FormUnavailable (FormTransportFailed "connection closed") -> pure (); _ -> error "cleanup erased original cause"
  assertEqual "lease closed on unavailable" [Opened,Awaited,Closed] seen
committedValue :: IO ()
committedValue = do
  let initial = (script [submitted "final" (String "o0")] [Right FormApplied]) {closeResult=Left (FormCleanupUnconfirmed "must never run")}
      (answer,seen) = runForm initial (choice "Action" (option (text "Original") (+1) :| []))
  case answer of Submitted action -> assertEqual "selected original survives native commit" (11::Int) (action 10); _ -> error "commit failed"
  assertEqual "native applied commit authoritatively settles the lease" [Opened,Awaited,Committed] seen
