{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- | Private construction and protocols for persistent Shoal agents.
module Tidepool.Actors.Internal.Agent
  ( AgentSpec
  , AgentRef
  , Response
  , Reply
  , codingAgent
  , readonlyAgent
  , readonlyWorktreeAgent
  , scaffoldingAgent
  , integrationAgent
  , startAgent
  , startForkedAgent
  , roleCode
  , request
  , requestSited
  , RequestOptions
  , Duration
  , RequestDeadline
  , milliseconds
  , seconds
  , minutes
  , after
  , requestOptions
  , withRequestGuidance
  , withRequestDeadline
  , requestWith
  , requestWithSited
  , requestWithProgress
  , requestWithProgressSited
  , requestWithProgressInto
  , requestWithProgressIntoSited
  , AgentState (..)
  , AgentObservation (..)
  , agentIdentity
  , agentBoundWorktree
  , observeAgent
  , lookupAgent
  , listAgents
  , AgentForgetOutcome (..)
  , forgetAgent
  , StopOutcome (..)
  , stopAgent
  , MessageRecipient
  , sendMessage
  , pollNotification
  , NotificationReceipt
  , NotificationError (..)
  , NotificationState (..)
  ) where

import Control.Monad.Freer (Eff, Member, raise, send)
import Data.Text (Text)
import Prelude

import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Internal as ActorInternal
import Tidepool.Actor.Source (installSource)
import Tidepool.Actors.Role (AgentControl, AgentInspection, AgentLaunch, Forks)
import Tidepool.Agent.Reply.Internal
  ( Reply
  , Progress (..)
  , responseRequestId
  , Replies
  , RequestLabel (..)
  , RequestId (..)
  , Response
  , fillResponse
  , newRequestHandles
  , reserveRequest
  , replyRequestId
  , submitRequest
  , ResponseResult (..)
  , ExecutionReceipt (..)
  , WorktreeEvidence (..)
  )
import Tidepool.Agent.Watch.Internal (WatchId (..))
import Tidepool.Agent.Session
  ( attachAgent
  , requestSessionSited
  )
import Tidepool.Effects.Core
  ( ActorContextInfo (..)
  , ActorEffectKey
  , ForkEffort
  , ForkContext
  , WorkerLifetime
  , ActorEffectProfile (..)
  , ActorKernel (..)
  , ActorLaunchRole (..)
  , AgentControl (..)
  , Notifications (..)
  , NotificationError (..)
  , NotificationState (..)
  , AgentStopControlOutcome (..)
  , AgentInspection (..)
  , AgentLaunch (..)
  , AgentRosterEntry (..)
  , AgentRosterState (..)
  , Forks (..)
  , WorktreeHandle (..)
  , WorktreeReceipt (..)
  , WorktreeSpec
  , DirtyPolicy
  )
import qualified Tidepool.Effects.Core as Core
import Tidepool.Internal.ExitCell (fillExitCell, newExitCell)
import Tidepool.Duration
  ( Duration
  , RequestDeadline
  , after
  , milliseconds
  , minutes
  , seconds
  )
import Tidepool.Worktree
  ( observeSubmission
  , renderBranchName
  , withWorktree
  , worktreeHead
  , worktreeId
  )

data AgentSpec
  = CodingAgent WorktreeHandle
  | ReadonlyAgent Text
  | ReadonlyWorktreeAgent WorktreeHandle
  | ScaffoldingAgent WorktreeHandle
  | IntegrationAgent WorktreeHandle

data AgentRef = AgentRef
  (Actor.ActorRef AgentProtocol ())
  (Maybe WorktreeHandle)

instance Show AgentRef where
  show agent = "AgentRef " <> show (agentIdentity agent)

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

agentIdentity :: AgentRef -> (Int, Int)
agentIdentity (AgentRef target _) = actorAddress target

agentBoundWorktree :: AgentRef -> Maybe WorktreeHandle
agentBoundWorktree (AgentRef _ tree) = tree

observeAgent
  :: Member AgentInspection effs
  => AgentRef
  -> Eff effs AgentObservation
observeAgent agent@(AgentRef target tree) = do
  roster <- inspectAgent target
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
lookupAgent (AgentRef target _) = inspectAgent target

rosterAgentState :: AgentRosterEntry -> AgentState
rosterAgentState entry = case rosterState entry of
  RosterRunning -> AgentRunning
  RosterStopped -> AgentStopped
  RosterFailed summary -> AgentFailed summary
  RosterCancelled summary -> AgentCancelled summary

listAgents :: Member AgentInspection effs => Eff effs [AgentRosterEntry]
listAgents = send AgentListWith

data AgentForgetOutcome
  = AgentForgotten
  | AgentForgetRunning
  | AgentForgetRetained [RequestId] [WatchId]
  | AgentForgetUnavailable
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

data AgentProtocol result where
  RunRequest
    :: Eff (Actor.ReadOnlyEffects AgentProtocol) ()
    -> AgentProtocol ()

data RequestOptions input = RequestOptions
  { requestOptionsLabel :: RequestLabel
  , requestOptionsInput :: input
  , requestOptionsGuidance :: Maybe Text
  , requestOptionsDeadline :: Maybe RequestDeadline
  }
  deriving (Show, Eq)

requestOptions :: RequestLabel -> input -> RequestOptions input
requestOptions label input = RequestOptions label input Nothing Nothing

withRequestGuidance :: Text -> RequestOptions input -> RequestOptions input
withRequestGuidance guidance options =
  options { requestOptionsGuidance = Just guidance }

withRequestDeadline :: RequestDeadline -> RequestOptions input -> RequestOptions input
withRequestDeadline deadline options =
  options { requestOptionsDeadline = Just deadline }

-- | Configure a long-lived coding agent around one managed worktree.
codingAgent :: WorktreeHandle -> AgentSpec
codingAgent = CodingAgent

-- | Configure a long-lived agent that shares the host checkout read-only.
readonlyAgent :: Text -> AgentSpec
readonlyAgent = ReadonlyAgent

readonlyWorktreeAgent :: WorktreeHandle -> AgentSpec
readonlyWorktreeAgent = ReadonlyWorktreeAgent

scaffoldingAgent :: WorktreeHandle -> AgentSpec
scaffoldingAgent = ScaffoldingAgent

integrationAgent :: WorktreeHandle -> AgentSpec
integrationAgent = IntegrationAgent

-- | Start one persistent Codex identity. Requests do not terminate it.
startAgent :: Member AgentLaunch effs => AgentSpec -> Eff effs AgentRef
startAgent spec = do
  actor <- launchFreshActor (agentDefinition spec) ()
  pure (AgentRef actor (agentWorktree spec))

-- | Start an agent by forking the caller's active provider and Haskell
-- snapshots. Public Shoal code reaches this through the applicative unfold DSL.
startForkedAgent
  :: Member Forks effs
  => Actor.LaunchRole
  -> Int
  -> Text
  -> Maybe WorktreeSpec
  -> DirtyPolicy
  -> [ActorEffectKey]
  -> Maybe ForkEffort
  -> Maybe (Int, Int)
  -> Maybe Text
  -> ForkContext
  -> Maybe Text
  -> WorkerLifetime
  -> Eff effs (Either Text (AgentRef, Text, WorktreeHandle))
startForkedAgent launchRole forkGroup actorLabel worktreeSpec dirtyPolicy effectKeys effort budget model context instructions lifetime = do
  launched <- launchForkedActor
    launchRole
    forkGroup
    (agentDefinitionUnbound actorLabel)
    ()
    worktreeSpec
    dirtyPolicy
    effectKeys
    effort
    budget
    model
    context
    instructions
    lifetime
  pure $ case launched of
    Left failure -> Left failure
    Right (actor, allocatedPath, tree) ->
      Right (AgentRef actor (Just tree), allocatedPath, tree)

-- | Submit a typed request and return its independently awaitable reply.
{-# OPAQUE request #-}
request
  :: forall result input effs
   . Member Replies effs
  => AgentRef
  -> RequestLabel
  -> input
  -> Eff effs (Response result)
request = requestSited @result @input 0

-- Extractor substrate. The caller's input and result monotypes are attached
-- to this site and reused by the target's external-agent session.
{-# OPAQUE requestSited #-}
requestSited
  :: forall result input effs
   . Member Replies effs
  => Int
  -> AgentRef
  -> RequestLabel
  -> input
  -> Eff effs (Response result)
requestSited site (AgentRef target targetWorktree) label@(RequestLabel renderedLabel) input = do
  requestConfiguredSited site target targetWorktree label Nothing Nothing input (const (pure ()))

{-# OPAQUE requestWith #-}
requestWith
  :: forall result input effs
   . Member Replies effs
  => AgentRef
  -> RequestOptions input
  -> Eff effs (Response result)
requestWith = requestWithSited @result @input 0

{-# OPAQUE requestWithProgress #-}
requestWithProgress
  :: forall progress result input effs
   . Member Replies effs
  => AgentRef
  -> RequestOptions input
  -> Eff effs (Response result, Progress progress)
requestWithProgress = requestWithProgressSited @progress @result @input 0

{-# OPAQUE requestWithProgressSited #-}
requestWithProgressSited
  :: forall progress result input effs
   . Member Replies effs
  => Int
  -> AgentRef
  -> RequestOptions input
  -> Eff effs (Response result, Progress progress)
requestWithProgressSited site actor options = do
  response <- requestWithSited @result @input site actor options
  pure (response, Progress (responseRequestId response))

-- | Retain the exact typed handles before admitting work. The callback should
-- transfer them to an owned mailbox or install their result route. If it fails,
-- no request is submitted; once admission happens its handles are already held
-- independently of the submitting handler's next state checkpoint.
{-# OPAQUE requestWithProgressInto #-}
requestWithProgressInto
  :: forall progress result input effs. Member Replies effs
  => AgentRef
  -> RequestOptions input
  -> ((Response result, Progress progress) -> Eff effs ())
  -> Eff effs (Response result, Progress progress)
requestWithProgressInto = requestWithProgressIntoSited @progress @result @input 0

{-# OPAQUE requestWithProgressIntoSited #-}
requestWithProgressIntoSited
  :: forall progress result input effs. Member Replies effs
  => Int
  -> AgentRef
  -> RequestOptions input
  -> ((Response result, Progress progress) -> Eff effs ())
  -> Eff effs (Response result, Progress progress)
requestWithProgressIntoSited site (AgentRef target targetWorktree) options retain = do
  let handles response = (response, Progress (responseRequestId response))
  response <- requestConfiguredSited site target targetWorktree
    (requestOptionsLabel options) (requestOptionsGuidance options)
    (requestOptionsDeadline options) (requestOptionsInput options) (retain . handles)
  pure (handles response)

{-# OPAQUE requestWithSited #-}
requestWithSited
  :: forall result input effs
   . Member Replies effs
  => Int
  -> AgentRef
  -> RequestOptions input
  -> Eff effs (Response result)
requestWithSited site (AgentRef target targetWorktree) options =
  requestConfiguredSited
    site
    target
    targetWorktree
    (requestOptionsLabel options)
    (requestOptionsGuidance options)
    (requestOptionsDeadline options)
    (requestOptionsInput options)
    (const (pure ()))

requestConfiguredSited
  :: forall result input effs
   . Member Replies effs
  => Int
  -> Actor.ActorRef AgentProtocol ()
  -> Maybe WorktreeHandle
  -> RequestLabel
  -> Maybe Text
  -> Maybe RequestDeadline
  -> input
  -> (Response result -> Eff effs ())
  -> Eff effs (Response result)
requestConfiguredSited site target targetWorktree label@(RequestLabel renderedLabel) guidance deadline input retain = do
  requestId <- reserveRequest label (actorAddress target)
  let (response, reply) = newRequestHandles input requestId
  retain response
  submitRequest
    requestId
    (actorAddress target)
    (RunRequest (runRequest (actorAddress target) targetWorktree response reply))
    deadline
  pure response
  where
    runRequest (targetActorId, targetIncarnation) targetTree response replyHandle = do
      let requestId = case replyRequestId replyHandle of
            RequestId value -> value
      start <- case targetTree of
        Nothing -> pure Nothing
        Just tree -> Just <$> worktreeHead tree
      result <-
        requestSessionSited @result @input
          site requestId (Just (activationGuidance renderedLabel guidance)) input
      evidence <- case (targetTree, start) of
        (Nothing, _) -> pure NoBoundWorktree
        (Just tree, Just startHead) -> do
          observed <- observeSubmission (worktreeId tree)
          pure $ case observed of
            Left failure -> WorktreeObservationFailed failure
            Right submission -> WorktreeObserved (handleReceipt tree) startHead submission
        (Just _, Nothing) -> error "bound worktree was not sampled"
      let execution = ExecutionReceipt
            { executionRequest = RequestId requestId
            , executionActorId = targetActorId
            , executionActorIncarnation = targetIncarnation
            }
      case fillResponse response (ResponseResult result execution evidence) of
        () -> pure ()

activationGuidance :: Text -> Maybe Text -> Text
activationGuidance label Nothing =
  label
activationGuidance label (Just guidance) =
  label <> "\n" <> guidance

-- | Observable result of asking one exact actor incarnation to retire.
--
-- Retirement is supervisor-owned and mailbox ordered. 'StoppedNow' means the
-- exact incarnation published its terminal state before the operation
-- returned. Repeating the operation is harmless and returns 'AlreadyStopped'.
data StopOutcome
  = StoppedNow
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
    AgentStopAlreadyStopped -> AlreadyStopped
    AgentStopUnavailable -> StopUnavailable
    AgentStopUnauthorized -> StopUnauthorized
    AgentStopFailed detail -> StopFailed detail

launchFreshActor
  :: forall effs startup api exit
   . Member AgentLaunch effs
  => Actor.ActorDefinition startup api exit
  -> startup
  -> Eff effs (Actor.ActorRef api exit)
launchFreshActor definition@Actor.ActorDefinition
  { Actor.label = actorLabel
  , Actor.effectProfile = profile
  , Actor.initialization = startupAction
  , Actor.behavior = install
  , Actor.onShutdown = shutdownAction
  } startup = do
  let cell = newExitCell startup
      shutdownEntry reasonCode =
        raiseActorKernel (shutdownAction (decodeShutdownReason reasonCode))
      entry _ = do
        send (ActorInstallShutdownWith 0 shutdownEntry)
        initial <- raiseActorKernel (startupAction startup)
        mapM_ installSource (ActorInternal.actorSources definition)
        send ActorReadyWith
        result <- raiseActorKernel (install startup initial)
        case fillExitCell cell result of
          () -> pure ()
  (actorId, incarnation, _) <- send
    (AgentLaunchWith
      actorLabel
      entry
      ActorInheritedRole
      (profileCode profile)
      (ActorInternal.actorLaunchWorktrees definition))
  pure (ActorInternal.ActorRef actorId incarnation cell)

launchForkedActor
  :: forall effs startup api exit
   . Member Forks effs
  => Actor.LaunchRole
  -> Int
  -> Actor.ActorDefinition startup api exit
  -> startup
  -> Maybe WorktreeSpec
  -> DirtyPolicy
  -> [ActorEffectKey]
  -> Maybe ForkEffort
  -> Maybe (Int, Int)
  -> Maybe Text
  -> ForkContext
  -> Maybe Text
  -> WorkerLifetime
  -> Eff effs (Either Text (Actor.ActorRef api exit, Text, WorktreeHandle))
launchForkedActor launchRole forkGroup definition@Actor.ActorDefinition
  { Actor.label = actorLabel
  , Actor.effectProfile = profile
  , Actor.initialization = startupAction
  , Actor.behavior = install
  , Actor.onShutdown = shutdownAction
  } startup worktreeSpec dirtyPolicy effectKeys effort budget model context instructions lifetime = do
  let cell = newExitCell startup
      shutdownEntry reasonCode =
        raiseActorKernel (shutdownAction (decodeShutdownReason reasonCode))
      entry _ = do
        send (ActorInstallShutdownWith 0 shutdownEntry)
        initial <- raiseActorKernel (startupAction startup)
        mapM_ installSource (ActorInternal.actorSources definition)
        send ActorReadyWith
        result <- raiseActorKernel (install startup initial)
        case fillExitCell cell result of
          () -> pure ()
  launched <- send
    (ForksStartWith
      actorLabel
      entry
      forkGroup
      (roleCode launchRole)
      (profileCode profile)
      (ActorInternal.actorLaunchWorktrees definition)
      worktreeSpec
      dirtyPolicy
      effectKeys
      effort
      budget
      model
      context
      instructions
      lifetime)
  pure $ case launched of
    Left failure -> Left failure
    Right ((actorId, incarnation, allocatedPath), tree) ->
      Right (ActorInternal.ActorRef actorId incarnation cell, allocatedPath, tree)

inspectAgent
  :: Member AgentInspection effs
  => Actor.ActorRef api exit
  -> Eff effs (Maybe AgentRosterEntry)
inspectAgent (ActorInternal.ActorRef actorId incarnation _) =
  send (AgentInspectWith (actorId, incarnation))

profileCode :: Actor.EffectProfile protocol effs -> ActorEffectProfile
profileCode = ActorInternal.profileCode

roleCode :: Actor.LaunchRole -> ActorLaunchRole
roleCode Actor.RootRole = ActorRootRole
roleCode Actor.ResearchRole = ActorResearchRole
roleCode Actor.CodingRole = ActorCodingRole
roleCode Actor.ScaffoldingRole = ActorScaffoldingRole
roleCode Actor.IntegrationRole = ActorIntegrationRole
roleCode Actor.InheritedRole = ActorInheritedRole

decodeShutdownReason :: Int -> Actor.ShutdownReason
decodeShutdownReason 0 = Actor.ShutdownCompleted
decodeShutdownReason 1 = Actor.ShutdownFailed
decodeShutdownReason _ = Actor.ShutdownCancelled

raiseActorKernel :: Eff effs a -> Eff (ActorKernel ': effs) a
raiseActorKernel = raise

agentWorktree :: AgentSpec -> Maybe WorktreeHandle
agentWorktree (CodingAgent tree) = Just tree
agentWorktree (ReadonlyAgent _) = Nothing
agentWorktree (ReadonlyWorktreeAgent tree) = Just tree
agentWorktree (ScaffoldingAgent tree) = Just tree
agentWorktree (IntegrationAgent tree) = Just tree

agentDefinition :: AgentSpec -> Actor.ActorDefinition () AgentProtocol ()
agentDefinition spec = agentDefinitionNamed (agentLabel spec) spec

agentDefinitionNamed :: Text -> AgentSpec -> Actor.ActorDefinition () AgentProtocol ()
agentDefinitionNamed actorLabel spec = attachWorktree spec (agentDefinitionUnbound actorLabel)

agentDefinitionUnbound :: Text -> Actor.ActorDefinition () AgentProtocol ()
agentDefinitionUnbound actorLabel = definition
  where
    definition =
      Actor.ActorDefinition
        { Actor.label = actorLabel
        , Actor.effectProfile = Actor.ReadOnly
        , Actor.initialization = \() -> attachAgent Nothing
        , Actor.behavior = \() () -> agentLoop
        , Actor.onShutdown = const (pure ())
        }

agentLoop :: Eff (Actor.ReadOnlyEffects AgentProtocol) ()
agentLoop = do
  continue <- Actor.receive handle
  if continue then agentLoop else pure ()
  where
    handle
      :: forall result
       . AgentProtocol result
      -> Eff (Actor.ReadOnlyEffects AgentProtocol) (result, Bool)
    handle (RunRequest action) = action >> pure ((), True)

actorAddress :: Actor.ActorRef protocol exit -> (Int, Int)
actorAddress (ActorInternal.ActorRef actorId incarnation _) =
  (actorId, incarnation)

agentLabel :: AgentSpec -> Text
agentLabel (CodingAgent tree) =
  "coding/" <> renderBranchName (branch (handleReceipt tree))
agentLabel (ReadonlyAgent label) = label
agentLabel (ReadonlyWorktreeAgent tree) =
  "research/" <> renderBranchName (branch (handleReceipt tree))
agentLabel (ScaffoldingAgent tree) =
  "scaffolding/" <> renderBranchName (branch (handleReceipt tree))
agentLabel (IntegrationAgent tree) =
  "integration/" <> renderBranchName (branch (handleReceipt tree))

attachWorktree
  :: AgentSpec
  -> Actor.ActorDefinition () AgentProtocol ()
  -> Actor.ActorDefinition () AgentProtocol ()
attachWorktree (CodingAgent tree) = withWorktree tree
attachWorktree (ReadonlyAgent _) = id
attachWorktree (ReadonlyWorktreeAgent tree) = withWorktree tree
attachWorktree (ScaffoldingAgent tree) = withWorktree tree
attachWorktree (IntegrationAgent tree) = withWorktree tree

agentRole :: AgentSpec -> Actor.LaunchRole
agentRole (ReadonlyAgent _) = Actor.ResearchRole
agentRole (ReadonlyWorktreeAgent _) = Actor.ResearchRole
agentRole (CodingAgent _) = Actor.CodingRole
agentRole (ScaffoldingAgent _) = Actor.ScaffoldingRole
agentRole (IntegrationAgent _) = Actor.IntegrationRole


-- | Observation locator only. Rust checks the caller and exact inbox row.
newtype NotificationReceipt = NotificationReceipt ((Int, Int), ((Int, Int), (Text, Int)))

-- | An address observation, not a grant. Rust checks the sending principal and
-- exact recipient. Capture actorContext in the model's turn before using it in
-- a child router. The handler runs as the router, not the capturing model.
class MessageRecipient recipient where
  messageAddress :: recipient -> (Int, Int)

instance MessageRecipient AgentRef where
  messageAddress = agentIdentity

instance MessageRecipient ActorContextInfo where
  messageAddress context = (contextActorId context, contextActorIncarnation context)

-- | Admit normal steering into the existing TUI conversation. The receipt is
-- admission evidence, not incorporation or successful execution. No response
-- obligation is created, and uncertain presentation must not be retried blindly.
sendMessage
  :: (MessageRecipient recipient, Member Notifications effs)
  => recipient -> Text -> Eff effs (Either NotificationError NotificationReceipt)
sendMessage recipient message =
  fmap (fmap NotificationReceipt) (send (NotifyWith (messageAddress recipient) message))

pollNotification :: Member Notifications effs => NotificationReceipt -> Eff effs (Either NotificationError NotificationState)
pollNotification (NotificationReceipt receipt) = send (PollNotificationWith receipt)
