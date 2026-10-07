{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- | Persistent-agent requests, observation and supervisor controls.
module Tidepool.Actors.Internal.Agent
  ( AgentRef
  , Request
  , request
  , requestSited
  , requestWithProgress
  , requestWithProgressSited
  , requestWithProgressInto
  , requestWithProgressIntoSited
  , RequestOptions (..)
  , defaultRequestOptions
  , RequestError (..)
  , SettlementReporting (..)
  , Duration, milliseconds, seconds, minutes
  , AgentState (..), AgentObservation (..)
  , agentIdentity, agentBoundWorktree, observeAgent, lookupAgent
  , AgentSummary (..), agentSummary, listAgents, listAgentsFull, findAgentsByLabel
  , AgentForgetOutcome (..), forgetAgent
  , StopOutcome (..), stopAgent
  , AgentRetentionError (..), retainAgent
  , sendMessage, parentAgent, pollNotification
  , installRequestReceiver, installRequestReceiverSited
  , NotificationReceipt, NotificationError (..), NotificationState (..)
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import qualified Data.Text as Text
import Prelude
import qualified Tidepool.Actor as Actor
import Tidepool.Agent.Reply.Internal
  ( Request, RequestOptions (..), defaultRequestOptions, RequestError (..)
  , SettlementReporting (..), Progress (..), responseRequestId
  , Replies, RequestId (..), fillResponse, newRequestHandles, reserveRequest
  , replyRequestId, submitRequest, abandonResponse
  , ResponseResult (..), ExecutionReceipt (..), WorktreeEvidence (..)
  )
import Tidepool.Agent.Ref.Internal
  ( AgentRef (..), AgentProtocol (..), agentIdentity, agentBoundWorktree, internalAgentRef )
import Tidepool.Agent.Watch.Internal (WatchId (..))
import Tidepool.Agent.Session (requestSessionSited)
import Tidepool.Inspection
  ( Display (..), DisplayRoot (..), DisplayTree (..), PageDisplay (..)
  , opaqueHandle, pageWithContinuation )
import Tidepool.Effects.Core
  ( AgentControl (..), AgentInspection (..), AgentTools, Notifications (..)
  , NotificationError (..), NotificationState (..), AgentStopControlOutcome (..)
  , WorkerLifetime, AgentRetentionError (..)
  , AgentDisposition (..), AgentRosterEntry (..), AgentRosterState (..)
  , WorktreeHandle (..), WorktreeReceipt (..) )
import qualified Tidepool.Effects.Core as Core
import Tidepool.Internal.ActorRef (actorAddress)
import Tidepool.Internal.RequestSite (RequestSite)
import Tidepool.Duration (Duration, milliseconds, minutes, seconds)
import Tidepool.Worktree (observeSubmission, worktreeHead, worktreeId)

-- | Register the fixed request receiver during child activation. The
-- receiver's effect row is carried by its own live callback, independent of
-- the selected effects used to install the child's tool policy.
{-# OPAQUE installRequestReceiver #-}
installRequestReceiver :: Member AgentTools effects => Eff effects ()
installRequestReceiver = installRequestReceiverSited
  (error "installRequestReceiver: extractor must assign a typed site")

{-# OPAQUE installRequestReceiverSited #-}
installRequestReceiverSited
  :: Member AgentTools effects
  => RequestSite '[] ()
  -> Eff effects ()
installRequestReceiverSited site = send
  (Core.AgentToolsInstallReceiverWith site agentRequestDriver)

agentRequestDriver :: Int -> Eff (Actor.ReadOnlyEffects AgentProtocol) ()
agentRequestDriver _ = Actor.serve @() @AgentProtocol ()
  (\() (RunRequest action) -> action >> pure ((), ()))

-- | Repeatable lifecycle observation of one exact actor incarnation.
data AgentState
  = AgentRunning
  | AgentStopped
  | AgentFailed Text
  | AgentCancelled Text
  | AgentUnavailable
  deriving (Show, Eq)

data AgentObservation = AgentObservation
  { observedAgentId :: Int
  , observedIncarnation :: Int
  , observedLabel :: Maybe Text
  , observedState :: AgentState
  , observedWorktree :: Maybe WorktreeHandle
  }
  deriving (Show, Eq)

observeAgent
  :: Member AgentInspection effs
  => AgentRef
  -> Eff effs AgentObservation
observeAgent agent@(AgentRef _ tree) = do
  roster <- lookupAgent agent
  let (actorId, incarnation) = agentIdentity agent
  pure AgentObservation
    { observedAgentId = actorId
    , observedIncarnation = incarnation
    , observedLabel = rosterLabel <$> roster
    , observedState = maybe AgentUnavailable rosterAgentState roster
    , observedWorktree = tree
    }

lookupAgent
  :: Member AgentInspection effs
  => AgentRef
  -> Eff effs (Maybe AgentRosterEntry)
lookupAgent (AgentRef target _) = send (AgentInspectWith (actorAddress target))

rosterAgentState :: AgentRosterEntry -> AgentState
rosterAgentState entry = case rosterState entry of
  RosterRunning -> AgentRunning
  RosterStopped -> AgentStopped
  RosterFailed summary -> AgentFailed summary
  RosterCancelled summary -> AgentCancelled summary

-- | What a supervisor reads when it asks who is out there: identity, what
-- model it asked for and what answered, whether the actor is running, what it
-- is doing, what it is doing it in, and when it started.
--
-- A roster entry carries everything the host observes, usage samples and
-- provider threading included. Five of those fill a screen, so the list view
-- is this projection and the whole record stays one 'lookupAgent' away.
data AgentSummary = AgentSummary
  { summaryActorId :: Int
  , summaryIncarnation :: Int
  , summaryLabel :: Text
  , summaryRequestedModel :: Maybe Text
  , summaryConfirmedModel :: Maybe Text
  , summaryState :: AgentRosterState
  , summaryDisposition :: Maybe AgentDisposition
  , summaryCurrentRequests :: [Int]
  , summaryBoundWorktree :: Maybe Text
  , summaryLaunchedAtUnixMs :: Maybe Int
  }
  deriving (Show, Eq)

agentSummary :: AgentRosterEntry -> AgentSummary
agentSummary entry = AgentSummary
  { summaryActorId = rosterActorId entry
  , summaryIncarnation = rosterActorIncarnation entry
  , summaryLabel = rosterLabel entry
  , summaryRequestedModel = rosterRequestedModel entry
  , summaryConfirmedModel = rosterConfirmedModel entry
  , summaryState = rosterState entry
  , summaryDisposition = rosterDisposition entry
  , summaryCurrentRequests = rosterCurrentRequests entry
  , summaryBoundWorktree = rosterBoundWorktree entry
  , summaryLaunchedAtUnixMs = rosterLaunchedAtUnixMs entry
  }

listAgents :: Member AgentInspection effs => Eff effs [AgentSummary]
listAgents = map agentSummary <$> listAgentsFull

-- | The unprojected roster. Callers that fold over the host's full
-- observation (usage totals, provider threading, context parentage) take this
-- one; a supervisor reading the roster takes 'listAgents'.
listAgentsFull :: Member AgentInspection effs => Eff effs [AgentRosterEntry]
listAgentsFull = send AgentListWith

-- | Every visible actor carrying this label, retired incarnations included,
-- so a reused or ambiguous label shows all its matches.
findAgentsByLabel :: Member AgentInspection effs => Text -> Eff effs [AgentRosterEntry]
findAgentsByLabel label = filter ((== label) . rosterLabel) <$> listAgentsFull

data AgentForgetOutcome
  = AgentForgotten
  | AgentForgetRunning
  | AgentForgetRetained [RequestId] [WatchId]
  | AgentForgetUnavailable
  | AgentForgetOutputPending Int
  deriving (Show, Eq)

forgetAgent :: Member AgentInspection effs => AgentRef -> Eff effs AgentForgetOutcome
forgetAgent (AgentRef target _) = do
  outcome <- send (AgentForgetWith (actorAddress target))
  pure $ case outcome of
    Core.AgentForgotten -> AgentForgotten
    Core.AgentForgetRunning -> AgentForgetRunning
    Core.AgentForgetRetained requests watches ->
      AgentForgetRetained (map RequestId requests) (map WatchId watches)
    Core.AgentForgetUnavailable -> AgentForgetUnavailable
    Core.AgentForgetOutputPending displays -> AgentForgetOutputPending displays

-- | Admit a request with raw typed input and one singular control handle.
{-# OPAQUE request #-}
request
  :: forall result input effs. Member Replies effs
  => AgentRef -> input -> RequestOptions
  -> Eff effs (Either RequestError (Request result))
request = requestSited (error "request: extractor must assign a typed site")

{-# OPAQUE requestSited #-}
requestSited
  :: forall result input effs. Member Replies effs
  => RequestSite '[input] result -> AgentRef -> input -> RequestOptions
  -> Eff effs (Either RequestError (Request result))
requestSited site agent input options =
  requestConfiguredSited site agent input options (const (pure ()))

{-# OPAQUE requestWithProgress #-}
requestWithProgress
  :: forall progress result input effs. Member Replies effs
  => AgentRef -> input -> RequestOptions
  -> Eff effs (Either RequestError (Request result, Progress progress))
requestWithProgress = requestWithProgressSited
  (error "requestWithProgress: extractor must assign a typed site")

{-# OPAQUE requestWithProgressSited #-}
requestWithProgressSited
  :: forall progress result input effs. Member Replies effs
  => RequestSite '[input, progress] result -> AgentRef -> input -> RequestOptions
  -> Eff effs (Either RequestError (Request result, Progress progress))
requestWithProgressSited site agent input options =
  requestWithProgressIntoSited site agent input options (const (pure ()))

-- | Retain both exact typed handles before publishing a request. If retaining
-- fails, admission has not occurred; the creating invocation rolls back the draft.
{-# OPAQUE requestWithProgressInto #-}
requestWithProgressInto
  :: forall progress result input effs. Member Replies effs
  => AgentRef -> input -> RequestOptions
  -> ((Request result, Progress progress) -> Eff effs ())
  -> Eff effs (Either RequestError (Request result, Progress progress))
requestWithProgressInto = requestWithProgressIntoSited
  (error "requestWithProgressInto: extractor must assign a typed site")

{-# OPAQUE requestWithProgressIntoSited #-}
requestWithProgressIntoSited
  :: forall progress result input effs. Member Replies effs
  => RequestSite '[input, progress] result -> AgentRef -> input -> RequestOptions
  -> ((Request result, Progress progress) -> Eff effs ())
  -> Eff effs (Either RequestError (Request result, Progress progress))
requestWithProgressIntoSited site agent input options retain = do
  let handles response = (response, Progress (responseRequestId response))
  submitted <- requestConfiguredSited site agent input options (retain . handles)
  pure (handles <$> submitted)

requestConfiguredSited
  :: forall result input extra effs. Member Replies effs
  => RequestSite (input ': extra) result -> AgentRef -> input -> RequestOptions
  -> (Request result -> Eff effs ())
  -> Eff effs (Either RequestError (Request result))
requestConfiguredSited site agent@(AgentRef target targetWorktree) input options retain = do
  reserved <- reserveRequest (requestLabel options) (actorAddress target)
    (requestReporting options) (requestLifetime options)
  case reserved of
    Left failure -> pure (Left failure)
    Right requestId -> do
      let (response, replyHandle) = newRequestHandles input requestId agent
      retain response
      admitted <- submitRequest requestId (actorAddress target)
        (RunRequest (runRequest targetWorktree response replyHandle)) (requestDeadline options)
      case admitted of
        Left failure -> do
          -- Submission refusal occurs before queue admission. Abandon eagerly;
          -- the creating invocation's rollback fence also owns this draft,
          -- independently of the requested cleanup lifetime.
          _ <- abandonResponse response
          pure (Left failure)
        Right () -> pure (Right response)
  where
    runRequest tree response replyHandle = do
      let requestId = case replyRequestId replyHandle of RequestId value -> value
          (actorId, incarnation) = actorAddress target
      start <- traverse worktreeHead tree
      result <- requestSessionSited @result @input site requestId (requestGuidance options) input
      evidence <- case (tree, start) of
        (Nothing, _) -> pure NoBoundWorktree
        (Just worktree, Just startHead) -> do
          observed <- observeSubmission (worktreeId worktree)
          pure $ case observed of
            Left failure -> WorktreeObservationFailed failure
            Right submission -> WorktreeObserved (handleReceipt worktree) startHead submission
        (Just _, Nothing) -> error "bound worktree was not sampled"
      let execution = ExecutionReceipt (RequestId requestId) actorId incarnation
      case fillResponse response (ResponseResult result execution evidence) of
        () -> pure ()

-- | Observable result of asking one exact actor incarnation to retire.
--
-- Retirement is supervisor-owned and mailbox ordered. Stopping has two
-- phases: the actor publishes its terminal state, then the host releases its
-- interactive resources (process, pane, tool service, socket, workspace
-- view). 'StoppedNow' means both happened before the operation returned.
-- 'StoppedRetaining' means the actor is stopped but the named resources stay
-- retained; 'StoppedReleasing' means release had not settled within the wait
-- and a later notice reports it. Repeating the operation is harmless and
-- returns 'AlreadyStopped'.
data StopOutcome
  = StoppedNow
  | StoppedRetaining Text
  | StoppedReleasing
  | AlreadyStopped
  | StopUnavailable
  | StopUnauthorized
  | StopFailed Text
  deriving (Show, Eq)

-- | Ask an agent to retire after all earlier mailbox requests settle.
-- The typed receipt makes retries and already-terminal handles explicit.
stopAgent
  :: Member AgentControl effs
  => AgentRef
  -> Eff effs StopOutcome
stopAgent (AgentRef target _) = do
  outcome <- send (AgentControlStopWith (actorAddress target))
  pure $ case outcome of
    AgentStoppedNow -> StoppedNow
    AgentStoppedRetaining detail -> StoppedRetaining detail
    AgentStoppedReleasing -> StoppedReleasing
    AgentStopAlreadyStopped -> AlreadyStopped
    AgentStopUnavailable -> StopUnavailable
    AgentStopUnauthorized -> StopUnauthorized
    AgentStopFailed detail -> StopFailed detail

-- | Transfer one exact actor's cleanup ownership atomically against closure.
retainAgent
  :: Member AgentControl effects
  => AgentRef -> WorkerLifetime -> Eff effects (Either AgentRetentionError ())
retainAgent agent lifetime = send (AgentControlRetainWith (agentIdentity agent) lifetime)

-- | Observation locator only. Rust checks the caller and exact inbox row.
newtype NotificationReceipt = NotificationReceipt ((Int, Int), ((Int, Int), (Text, Int)))
  deriving (Show, Eq)

-- | Nested in another value: which notification, to whom.
instance Display NotificationReceipt where
  displayTree receipt =
    opaqueHandle ("notification " <> notificationSummary receipt)

-- | Explicit top-level output describes admission, never model presentation.
instance DisplayRoot NotificationReceipt where
  displayRoot = TextLeaf . notificationAccepted

instance DisplayRoot (Either NotificationError NotificationReceipt) where
  displayRoot (Right receipt) = displayRoot receipt
  displayRoot (Left failure) =
    Concat [TextLeaf "notification not accepted: ", StringLeaf (show failure)]

-- | A retained pure page uses the same admission description.
instance PageDisplay effects NotificationReceipt where
  displayPage budget receipt =
    pageWithContinuation budget (TextLeaf (notificationAccepted receipt)) Nothing

-- | 'sendMessage''s result, bound or observed as a cell's value.
instance PageDisplay effects (Either NotificationError NotificationReceipt) where
  displayPage budget (Right receipt) = displayPage budget receipt
  displayPage budget (Left failure) =
    pageWithContinuation budget
      (Concat [TextLeaf "notification not accepted: ", displayTree failure]) Nothing

notificationSummary :: NotificationReceipt -> Text
notificationSummary (NotificationReceipt (_, ((target, incarnation), (_, sequence)))) =
  Text.pack (show sequence) <> " to agent "
    <> Text.pack (show target) <> "@" <> Text.pack (show incarnation)

notificationAccepted :: NotificationReceipt -> Text
notificationAccepted receipt =
  "notification " <> notificationSummary receipt
    <> " accepted; `pollNotification` on this receipt reports whether it was presented"

-- | Admit normal steering into the existing TUI conversation. The receipt is
-- admission evidence, not incorporation or successful execution. No response
-- obligation is created, and uncertain presentation must not be retried blindly.
sendMessage
  :: Member Notifications effs
  => AgentRef -> Text -> Eff effs (Either NotificationError NotificationReceipt)
sendMessage recipient message =
  fmap (fmap NotificationReceipt) (send (NotifyWith (agentIdentity recipient) message))

-- | The supervising actor: it receives this actor's 'sendMessage' and
-- settles its request. Every child has one, whether its context was
-- inherited or selected; a root answers 'Nothing'. The reference carries bare
-- identity, no bound worktree.
parentAgent :: Member Core.ActorContext effs => Eff effs (Maybe AgentRef)
parentAgent = do
  context <- Core.actorContext
  pure $ case (Core.contextSupervisorId context, Core.contextSupervisorIncarnation context) of
    (Just parent, Just incarnation) -> Just (internalAgentRef parent incarnation)
    _ -> Nothing

pollNotification :: Member Notifications effs => NotificationReceipt -> Eff effs (Either NotificationError NotificationState)
pollNotification (NotificationReceipt receipt) = send (PollNotificationWith receipt)
