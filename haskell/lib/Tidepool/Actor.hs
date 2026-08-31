{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- | Typed observation of exact actor incarnations.
--
-- Copies of an 'ActorRef' observe the same immutable terminal result.
-- Successful @exit@ values may be arbitrary Haskell values, including
-- closures and lazy structures; 'awaitExit' is repeatable and never
-- serializes them.
--
-- Target failure and cancellation are domain-visible lifecycle outcomes.
-- An invalid reference, a machine-boundary violation, or cancellation of the
-- waiter is a runtime failure and does not fabricate an 'ActorExit'.
module Tidepool.Actor
  ( ActorRef
  , ActorDefinition (..)
  , startActor
  , runActor
  , ActorExit (..)
  , ActorFailure (..)
  , CancelReason (..)
  , awaitExit
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Prelude

import Tidepool.Actor.Internal
  ( ActorDefinition (..)
  , ActorRef (..)
  )
import Tidepool.Effects.Core
  ( Actor (..)
  , ActorLocal (..)
  , ActorTerminalStatus (..)
  )
import Tidepool.Internal.ExitCell
  ( fillExitCell
  , newExitCell
  , readExitCell
  )

-- | Failure reported for a terminated actor.
newtype ActorFailure = ActorFailure { actorFailureSummary :: Text }
  deriving (Show, Eq)

-- | Why an actor was cancelled rather than completing or failing.
newtype CancelReason = CancelReason { cancelReasonSummary :: Text }
  deriving (Show, Eq)

-- | The immutable terminal result retained by one exact 'ActorRef'.
data ActorExit exit
  = Completed exit
  | Failed ActorFailure
  | Cancelled CancelReason
  deriving (Show, Eq)

-- | Start a fresh actor from an authored definition. Tidepool captures the
-- definition's exact Haskell environment internally; authored code does not
-- manage a separate deployment value or preparation step.
--
-- Keep any 'deliberate' call in the definition's initialization function:
-- that concrete call site is where GHC records its exact input and output
-- types. This operation returns only after the child has installed its
-- behavior and reached readiness.
{-# NOINLINE startActor #-}
startActor
  :: forall effs startup api exit
   . Member Actor effs
  => ActorDefinition startup api exit
  -> startup
  -> Eff effs (ActorRef api exit)
startActor (ActorDefinition label startupAction install) startup = do
  let cell = newExitCell startup
      entry _ = do
        initial <- startupAction startup
        send (ActorReadyWith @api @exit)
        result <- install startup initial
        case fillExitCell cell result of
          () -> pure ()
  (actorId, incarnation) <- send (ActorStartWith label entry)
  pure (ActorRef actorId incarnation cell)

-- | Start a supervised one-shot actor and wait for its exact terminal value.
runActor
  :: Member Actor effs
  => ActorDefinition startup api exit
  -> startup
  -> Eff effs (ActorExit exit)
runActor definition startup = startActor definition startup >>= awaitExit

-- | Observe this exact actor incarnation's retained terminal result.
--
-- Completion is published into the reference's cell before the Rust terminal
-- transition. Seeing completion with an empty cell is therefore an engine
-- invariant violation, not a lifecycle case authors must handle.
awaitExit :: Member Actor effs => ActorRef api exit -> Eff effs (ActorExit exit)
awaitExit (ActorRef actorId incarnation cell) = do
  terminal <- send (ActorWaitWith (actorId, incarnation))
  case terminal of
    ActorCompletedStatus ->
      case readExitCell terminal cell of
        Just value -> pure (Completed value)
        Nothing -> error "Tidepool.Actor.awaitExit: completed actor has an empty exit cell"
    ActorFailedStatus summary -> pure (Failed (ActorFailure summary))
    ActorCancelledStatus summary -> pure (Cancelled (CancelReason summary))
