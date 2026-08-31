{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

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
  , ActorSpec
  , actorSpec
  , startActor
  , runActor
  , ActorExit (..)
  , ActorFailure (..)
  , CancelReason (..)
  , awaitExit
  ) where

import Control.Monad.Freer (Eff, Member, Members, send)
import Data.Text (Text)
import Prelude

import Tidepool.Actor.Internal
  ( ActorSpec (..)
  , AgentRef (..)
  )
import Tidepool.Effects
  ( Actor (..)
  , ActorLocal (..)
  , ActorTerminalStatus (..)
  , Deliberate
  )
import Tidepool.Internal.ExitCell
  ( fillExitCell
  , newExitCell
  , readExitCell
  )

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

-- | Define a prompted actor. The startup model produces a typed
-- initialization value; the pure installer decides how that value configures
-- the fixed authored program. Keep the 'deliberate' call in the supplied
-- startup function: that concrete authored call site is where GHC records the
-- exact input and output types before the row is hidden by 'ActorSpec'.
actorSpec
  :: Members '[Deliberate, ActorLocal api exit] actorEffs
  => Text
  -> (startup -> Eff actorEffs initial)
  -> (startup -> initial -> Eff actorEffs exit)
  -> ActorSpec startup api exit
actorSpec = ActorSpec

-- | Start a fresh actor and return only after its program has been installed
-- and reached the runtime readiness boundary.
{-# NOINLINE startActor #-}
startActor
  :: forall effs startup api exit
   . Member Actor effs
  => ActorSpec startup api exit
  -> startup
  -> Eff effs (AgentRef api exit)
startActor (ActorSpec label startupAction install) startup = do
  let cell = newExitCell startup
      entry _ = do
        initial <- startupAction startup
        send (ActorReadyWith @api @exit)
        result <- install startup initial
        case fillExitCell cell result of
          () -> pure ()
  (actorId, incarnation) <- send (ActorStartWith label entry)
  pure (AgentRef actorId incarnation cell)

-- | Start a supervised one-shot actor and wait for its exact terminal value.
runActor
  :: Member Actor effs
  => ActorSpec startup api exit
  -> startup
  -> Eff effs (ActorExit exit)
runActor spec startup = startActor spec startup >>= awaitExit

-- | Observe this exact actor incarnation's retained terminal result.
--
-- Completion is published into the reference's cell before the Rust terminal
-- transition. Seeing completion with an empty cell is therefore an engine
-- invariant violation, not a lifecycle case authors must handle.
awaitExit :: Member Actor effs => AgentRef api exit -> Eff effs (ActorExit exit)
awaitExit (AgentRef actorId incarnation cell) = do
  terminal <- send (ActorWaitWith (actorId, incarnation))
  case terminal of
    ActorCompletedStatus ->
      case readExitCell terminal cell of
        Just value -> pure (Completed value)
        Nothing -> error "Tidepool.Actor.awaitExit: completed actor has an empty exit cell"
    ActorFailedStatus summary -> pure (Failed (ActorFailure summary))
    ActorCancelledStatus summary -> pure (Cancelled (CancelReason summary))
