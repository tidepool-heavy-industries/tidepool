{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}

-- | Typed observation of exact actor incarnations.
--
-- An 'AgentRef' is copyable ordinary Haskell data. Rust retains its immutable
-- terminal metadata; the reference itself shares a managed Haskell cell that
-- retains a successful @exit@ value, including closures and lazy structures.
-- Consequently 'awaitExit' is repeatable and never serializes the exit value.
--
-- Target failure and cancellation are domain-visible lifecycle outcomes.
-- An invalid reference, a machine-boundary violation, or cancellation of the
-- waiter is a runtime failure and does not fabricate an 'ActorExit'.
module Tidepool.Actor
  ( AgentRef
  , ActorExit (..)
  , ActorFailure (..)
  , CancelReason (..)
  , awaitExit
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Prelude

import Tidepool.Actor.Internal (AgentRef (..))
import Tidepool.Effects (Actor (..), ActorTerminalStatus (..))
import Tidepool.Internal.ExitCell (readExitCell)

-- | Runtime failure reported for a terminated actor. The structured Rust
-- record remains authoritative; this first surface carries its concise
-- model-facing summary and can grow only when Haskell needs a policy choice.
newtype ActorFailure = ActorFailure { actorFailureSummary :: Text }
  deriving (Show, Eq)

-- | Why an actor was cancelled rather than completing or failing.
newtype CancelReason = CancelReason { cancelReasonSummary :: Text }
  deriving (Show, Eq)

-- | The immutable terminal result retained by one exact 'AgentRef'.
data ActorExit exit
  = Completed exit
  | Failed ActorFailure
  | Cancelled CancelReason
  deriving (Show, Eq)

-- | Observe this exact actor incarnation's retained terminal result.
--
-- Completion is published into the reference's cell before the Rust terminal
-- transition. Seeing completion with an empty cell is therefore an engine
-- invariant violation, not a lifecycle case authors must handle.
awaitExit :: Member Actor effs => AgentRef api exit -> Eff effs (ActorExit exit)
awaitExit (AgentRef actorId incarnation cell) = do
  terminal <- send (ActorWaitWith actorId incarnation)
  case terminal of
    ActorCompletedStatus ->
      case readExitCell terminal cell of
        Just value -> pure (Completed value)
        Nothing -> error "Tidepool.Actor.awaitExit: completed actor has an empty exit cell"
    ActorFailedStatus summary -> pure (Failed (ActorFailure summary))
    ActorCancelledStatus summary -> pure (Cancelled (CancelReason summary))
