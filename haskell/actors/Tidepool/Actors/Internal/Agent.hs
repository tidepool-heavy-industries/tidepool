{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

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
  , request
  , requestSited
  , AgentState (..)
  , AgentObservation (..)
  , agentIdentity
  , agentBoundWorktree
  , observeAgent
  , StopOutcome (..)
  , stopAgent
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Prelude

import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Internal as ActorInternal
import Tidepool.Actors.Role (AgentControl, AgentInspection, AgentLaunch)
import Tidepool.Agent.Reply.Internal
  ( Reply
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
import Tidepool.Agent.Session
  ( attachAgent
  , requestSessionSited
  )
import Tidepool.Effects.Core (Actor, WorktreeHandle (..), WorktreeReceipt (..))
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

-- | Repeatable lifecycle observation of one exact actor incarnation.
data AgentState
  = AgentRunning
  | AgentStopped
  | AgentFailed Text
  | AgentCancelled Text
  deriving (Show, Eq)

data AgentObservation = AgentObservation
  { observedAgentId :: Int
  , observedIncarnation :: Int
  , observedState :: AgentState
  , observedWorktree :: Maybe WorktreeHandle
  }
  deriving (Show, Eq)

agentIdentity :: AgentRef -> (Int, Int)
agentIdentity (AgentRef target _) = actorAddress target

agentBoundWorktree :: AgentRef -> Maybe WorktreeHandle
agentBoundWorktree (AgentRef _ tree) = tree

observeAgent
  :: (Member AgentInspection effs, Member Actor effs)
  => AgentRef
  -> Eff effs AgentObservation
observeAgent agent@(AgentRef target tree) = do
  terminal <- Actor.pollExit target
  let (actorId, incarnation) = agentIdentity agent
  pure AgentObservation
    { observedAgentId = actorId
    , observedIncarnation = incarnation
    , observedState = case terminal of
        Nothing -> AgentRunning
        Just (Actor.Completed ()) -> AgentStopped
        Just (Actor.Failed failure) -> AgentFailed (Actor.actorFailureSummary failure)
        Just (Actor.Cancelled reason) -> AgentCancelled (Actor.cancelReasonSummary reason)
    , observedWorktree = tree
    }

data AgentProtocol result where
  RunRequest
    :: Eff (Actor.ReadOnlyEffects AgentProtocol) ()
    -> AgentProtocol ()
  StopAgent :: AgentProtocol ()

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
startAgent :: (Member AgentLaunch effs, Member Actor effs) => AgentSpec -> Eff effs AgentRef
startAgent spec = do
  actor <- Actor.startActor (agentDefinition spec) ()
  pure (AgentRef actor (agentWorktree spec))

-- | Start an agent by forking the caller's active provider and Haskell
-- snapshots. Public Shoal code reaches this through the applicative unfold DSL.
startForkedAgent :: Member Actor effs => Int -> Text -> AgentSpec -> Eff effs (AgentRef, Text)
startForkedAgent forkGroup actorLabel spec = do
  (actor, allocatedPath) <- Actor.startActorFork (agentRole spec) forkGroup (agentDefinitionNamed actorLabel spec) ()
  pure (AgentRef actor (agentWorktree spec), allocatedPath)

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
  requestId <- reserveRequest label (actorAddress target)
  let (response, reply) = newRequestHandles input requestId
  submitRequest
    requestId
    (actorAddress target)
    (RunRequest (runRequest (actorAddress target) targetWorktree response reply))
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
          site requestId (Just ("Continue request `" <> renderedLabel <> "` using the shared context and mounted sessionInput.")) input
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

-- | Observable result of asking one exact actor incarnation to retire.
--
-- A stop request is cooperative and mailbox ordered. 'StopRequested' means
-- the target accepted that request; lifecycle observation may still briefly
-- report it as running. Repeating the operation after terminal publication is
-- harmless and returns 'AlreadyStopped'.
data StopOutcome
  = StopRequested
  | AlreadyStopped (Actor.ActorExit ())
  | StopFailed Text
  deriving (Show, Eq)

-- | Ask an agent to retire after all earlier mailbox requests settle.
-- The typed receipt makes retries and already-terminal handles explicit.
stopAgent
  :: (Member AgentControl effs, Member Actor effs)
  => AgentRef
  -> Eff effs StopOutcome
stopAgent (AgentRef target _) = do
  before <- Actor.pollExit target
  case before of
    Just terminal -> pure (AlreadyStopped terminal)
    Nothing -> do
      requested <- ActorInternal.tryCallUnit target StopAgent
      case requested of
        Right () -> pure StopRequested
        Left failure -> do
          after <- Actor.pollExit target
          pure $ case after of
            Just terminal -> AlreadyStopped terminal
            Nothing -> StopFailed failure

agentWorktree :: AgentSpec -> Maybe WorktreeHandle
agentWorktree (CodingAgent tree) = Just tree
agentWorktree (ReadonlyAgent _) = Nothing
agentWorktree (ReadonlyWorktreeAgent tree) = Just tree
agentWorktree (ScaffoldingAgent tree) = Just tree
agentWorktree (IntegrationAgent tree) = Just tree

agentDefinition :: AgentSpec -> Actor.ActorDefinition () AgentProtocol ()
agentDefinition spec = agentDefinitionNamed (agentLabel spec) spec

agentDefinitionNamed :: Text -> AgentSpec -> Actor.ActorDefinition () AgentProtocol ()
agentDefinitionNamed actorLabel spec = attachWorktree spec definition
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
    handle StopAgent = pure ((), False)

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
