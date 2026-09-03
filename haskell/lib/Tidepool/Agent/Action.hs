{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# OPTIONS_GHC -Wno-simplifiable-class-constraints #-}

-- | Haskell actions returned by an interactive agent session.
--
-- An agent may return ordinary effectful Haskell instead of waiting inside
-- its tool call. The resident actor runs that value after the tool call has
-- settled. 'nextTurn' opens a new turn in the same agent context
-- with the action's typed result mounted as @sessionInput@.
module Tidepool.Agent.Action
  ( AgentAction (..)
  , ActionFailure (..)
  , liftAction
  , waitOn
  , nextTurn
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Prelude

import Tidepool.Actor
  ( ActorExit (..)
  , ActorRef
  , actorFailureSummary
  , awaitExit
  , cancelReasonSummary
  )
import Tidepool.Agent.Session (pattern ActionCompleted, agentSessionSited)
import Tidepool.Effects.Core (Actor, AgentSession)

-- | Why a model-authored continuation could not produce its value.
--
-- This is deliberately narrower than actor lifecycle. 'awaitExit' remains
-- available when policy needs to branch over the complete lifecycle sum;
-- 'waitOn' is the compositional success path.
data ActionFailure
  = AwaitedActorFailed Text
  | AwaitedActorCancelled Text
  deriving (Show, Eq)

-- | A live Haskell action returned by an agent into its resident actor.
--
-- The actor keeps its ordinary effect row.  This wrapper adds only local,
-- typed short-circuiting for helpers such as 'waitOn'; it does not introduce
-- another runtime effect or interpreter boundary.
newtype AgentAction effs result = AgentAction
  { runAgentAction :: Eff effs (Either ActionFailure result)
  }

instance Functor (AgentAction effs) where
  fmap f (AgentAction action) = AgentAction (fmap (fmap f) action)

instance Applicative (AgentAction effs) where
  pure = AgentAction . pure . Right
  AgentAction function <*> AgentAction argument = AgentAction $ do
    functionResult <- function
    case functionResult of
      Left failure -> pure (Left failure)
      Right f -> fmap (fmap f) argument

instance Monad (AgentAction effs) where
  AgentAction action >>= next = AgentAction $ do
    result <- action
    case result of
      Left failure -> pure (Left failure)
      Right value -> runAgentAction (next value)

-- | Lift an ordinary effectful computation into a compositional agent action.
liftAction :: Eff effs result -> AgentAction effs result
liftAction = AgentAction . fmap Right

-- | Wait for the successful value of an already-started exact actor.
--
-- Ordinary @(<*>)@ is sufficient for fan-in: every referenced actor is
-- already running, even though the resulting waits are observed in normal
-- Haskell evaluation order. Use 'awaitExit' instead when lifecycle failure is
-- part of the authored domain policy.
waitOn :: Member Actor effs => ActorRef protocol exit -> AgentAction effs exit
waitOn ref = AgentAction $ do
  outcome <- awaitExit ref
  pure $ case outcome of
    Completed value -> Right value
    Failed failure -> Left (AwaitedActorFailed (actorFailureSummary failure))
    Cancelled reason -> Left (AwaitedActorCancelled (cancelReasonSummary reason))

-- Extractor substrate. Fully-applied public calls are rewritten with their
-- concrete GHC-derived input and result types.
{-# OPAQUE continueWithSited #-}
continueWithSited
  :: forall result input effs
   . Member AgentSession effs
  => Int
  -> input
  -> AgentAction effs result
continueWithSited site input = AgentAction $ do
  continuation <-
    agentSessionSited @(AgentAction effs result) @input
      site
      ActionCompleted
      Nothing
      input
  runAgentAction continuation

-- | Run an action and make its result the next turn's typed @sessionInput@.
{-# OPAQUE nextTurn #-}
nextTurn
  :: forall result input effs
   . Member AgentSession effs
  => AgentAction effs input
  -> AgentAction effs result
nextTurn = nextTurnSited @result @input 0

-- Extractor substrate for 'nextTurn'. The eventual session receives the
-- successful value produced by the supplied action, so the site's live input
-- type is @input@ rather than @AgentAction effs input@.
{-# OPAQUE nextTurnSited #-}
nextTurnSited
  :: forall result input effs
   . Member AgentSession effs
  => Int
  -> AgentAction effs input
  -> AgentAction effs result
nextTurnSited site action = action >>= continueWithSited @result @input site
