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
  , startAgent
  , request
  , requestSited
  , stopAgent
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Prelude

import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Internal as ActorInternal
import Tidepool.Agent.Reply.Internal
  ( Reply
  , Replies
  , RequestId (..)
  , Response
  , fillResponse
  , newRequestHandles
  , reserveRequest
  , replyRequestId
  , submitRequest
  , ResponseResult (..)
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

data AgentRef = AgentRef
  (Actor.ActorRef AgentProtocol ())
  (Maybe WorktreeHandle)

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

-- | Start one persistent Codex identity. Requests do not terminate it.
startAgent :: Member Actor effs => AgentSpec -> Eff effs AgentRef
startAgent spec = do
  actor <- Actor.startActor (agentDefinition spec) ()
  pure (AgentRef actor (agentWorktree spec))

-- | Submit a typed request and return its independently awaitable reply.
{-# OPAQUE request #-}
request
  :: forall result input effs
   . Member Replies effs
  => AgentRef
  -> Text
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
  -> Text
  -> input
  -> Eff effs (Response result)
requestSited site (AgentRef target targetWorktree) prompt input = do
  requestId <- reserveRequest (actorAddress target)
  let (response, reply) = newRequestHandles input requestId
  submitRequest
    requestId
    (actorAddress target)
    (RunRequest (runRequest targetWorktree response reply))
  pure response
  where
    runRequest targetTree response replyHandle = do
      let requestId = case replyRequestId replyHandle of
            RequestId value -> value
      start <- case targetTree of
        Nothing -> pure Nothing
        Just tree -> Just <$> worktreeHead tree
      result <-
        requestSessionSited @result @input
          site requestId (Just prompt) input
      evidence <- case (targetTree, start) of
        (Nothing, _) -> pure NoBoundWorktree
        (Just tree, Just startHead) -> do
          observed <- observeSubmission (worktreeId tree)
          pure $ case observed of
            Left failure -> WorktreeObservationFailed failure
            Right submission -> WorktreeObserved startHead submission
        (Just _, Nothing) -> error "bound worktree was not sampled"
      case fillResponse response (ResponseResult result evidence) of
        () -> pure ()

-- | Ask an agent to retire after all earlier mailbox requests settle.
stopAgent :: Member Actor effs => AgentRef -> Eff effs ()
stopAgent (AgentRef target _) = Actor.cast target StopAgent

agentWorktree :: AgentSpec -> Maybe WorktreeHandle
agentWorktree (CodingAgent tree) = Just tree
agentWorktree (ReadonlyAgent _) = Nothing

agentDefinition :: AgentSpec -> Actor.ActorDefinition () AgentProtocol ()
agentDefinition spec = attachWorktree spec definition
  where
    definition =
      Actor.ActorDefinition
        { Actor.label = agentLabel spec
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

attachWorktree
  :: AgentSpec
  -> Actor.ActorDefinition () AgentProtocol ()
  -> Actor.ActorDefinition () AgentProtocol ()
attachWorktree (CodingAgent tree) = withWorktree tree
attachWorktree (ReadonlyAgent _) = id
