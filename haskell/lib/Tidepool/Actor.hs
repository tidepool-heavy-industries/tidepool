{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE RankNTypes #-}
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
  , ActorDefinition
  , pattern ActorDefinition
  , label
  , effectProfile
  , initialization
  , behavior
  , visibleToChild
  , onShutdown
  , EffectProfile (..)
  , ReadOnlyEffects
  , ReadWriteEffects
  , ShutdownReason (..)
  , startActor
  , runActor
  , call
  , cast
  , receive
  , serve
  , ActorExit (..)
  , ActorFailure (..)
  , CancelReason (..)
  , awaitExit
  , pollExit
  ) where

import Control.Monad.Freer (Eff, Member, raise, send)
import Data.Text (Text)
import Prelude

import Tidepool.Actor.Internal
  ( ActorDefinition
  , pattern ActorDefinition
  , label
  , effectProfile
  , initialization
  , behavior
  , visibleToChild
  , onShutdown
  , actorLaunchWorktrees
  , ActorRef (..)
  , EffectProfile (..)
  , ReadOnlyEffects
  , ReadWriteEffects
  , ShutdownReason (..)
  )
import Tidepool.Effects.Core
  ( Actor (..)
  , ActorKernel (..)
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
startActor definition@ActorDefinition
  { label = actorLabel
  , effectProfile = profile
  , initialization = startupAction
  , behavior = install
  , visibleToChild = exports
  , onShutdown = shutdownAction
  } startup = do
  let cell = newExitCell startup
      shutdownEntry reasonCode =
        raiseKernel (shutdownAction (decodeShutdownReason reasonCode))
      entry _ = do
        send (ActorInstallShutdownWith 0 shutdownEntry)
        initial <- raiseKernel (startupAction startup)
        send ActorReadyWith
        result <- raiseKernel (install startup initial)
        case fillExitCell cell result of
          () -> pure ()
  (actorId, incarnation) <- send
    (ActorStartWith actorLabel entry (profileCode profile) (actorLaunchWorktrees definition) exports)
  pure (ActorRef actorId incarnation cell)

profileCode :: EffectProfile protocol effs -> Int
profileCode ReadWrite = 0
profileCode ReadOnly = 1

decodeShutdownReason :: Int -> ShutdownReason
decodeShutdownReason 0 = ShutdownCompleted
decodeShutdownReason 1 = ShutdownFailed
decodeShutdownReason _ = ShutdownCancelled

raiseKernel :: Eff effs a -> Eff (ActorKernel ': effs) a
raiseKernel = raise

-- | Start a supervised one-shot actor and wait for its exact terminal value.
runActor
  :: Member Actor effs
  => ActorDefinition startup api exit
  -> startup
  -> Eff effs (ActorExit exit)
runActor definition startup = startActor definition startup >>= awaitExit

-- | Make one synchronous request to an exact actor incarnation. Runtime
-- lifecycle failure abandons this fragment; it is never fabricated as a
-- value of the protocol's result type.
call
  :: Member Actor effs
  => ActorRef protocol exit
  -> protocol result
  -> Eff effs result
call (ActorRef actorId incarnation _) request =
  send (ActorCallWith (actorId, incarnation) request)

-- | Transfer one unit-result request into an exact actor mailbox. Returning
-- means the mailbox accepted ownership, not that the target handled it.
cast
  :: Member Actor effs
  => ActorRef protocol exit
  -> protocol ()
  -> Eff effs ()
cast (ActorRef actorId incarnation _) request =
  send (ActorCastWith (actorId, incarnation) request)

-- | Handle one request of any result index and return the next actor state.
-- Reply authority remains in the runtime; authored code returns an ordinary
-- pair and never receives a token it could duplicate or forget.
{-# OPAQUE receive #-}
receive
  :: forall next protocol effs
   . Member (ActorLocal protocol) effs
  => (forall result. protocol result -> Eff effs (result, next))
  -> Eff effs next
receive = receiveSited @next 0

-- Extractor substrate. The stable site key correlates the receive suspension
-- with its two private settlement steps and gives observability a source-level
-- identity without exposing a reply token.
{-# OPAQUE receiveSited #-}
receiveSited
  :: forall next protocol effs
   . Member (ActorLocal protocol) effs
  => Int
  -> (forall result. protocol result -> Eff effs (result, next))
  -> Eff effs next
receiveSited site handler = send (ActorReceiveWith site kernelHandler)
  where
    kernelHandler :: forall result. protocol result -> Eff (ActorKernel ': effs) ()
    kernelHandler request = do
      (reply, next) <- raiseKernel (handler request)
      send (ActorReplyWith site reply)
      send (ActorContinueWith site next)

-- | Serve requests forever with explicit Haskell state. Actors that may exit
-- in response to a message use 'receive' directly and return normally.
serve
  :: forall state protocol effs exit
   . Member (ActorLocal protocol) effs
  => state
  -> (forall result. state -> protocol result -> Eff effs (result, state))
  -> Eff effs exit
{-# OPAQUE serve #-}
serve state step = serveSited @state unreachableSiteId state step
  where
    unreachableSiteId = error
      "serve: unreachable — extract must assign its receive site at the fully applied call"

-- A recursive server owns one stable receive site. The extractor rewrites the
-- public, fully-applied `serve` call where `state` is concrete; recursion then
-- reuses that site instead of trying to extract this polymorphic library body.
{-# OPAQUE serveSited #-}
serveSited
  :: forall state protocol effs exit
   . Member (ActorLocal protocol) effs
  => Int
  -> state
  -> (forall result. state -> protocol result -> Eff effs (result, state))
  -> Eff effs exit
serveSited site state step =
  receiveSited @state site (step state) >>= \next -> serveSited site next step

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

-- | Observe an exact actor without parking the caller. Completion reads the
-- same shared exit cell as 'awaitExit'; repeated polls therefore return the
-- same typed result and never consume custody.
pollExit :: Member Actor effs => ActorRef api exit -> Eff effs (Maybe (ActorExit exit))
pollExit (ActorRef actorId incarnation cell) = do
  terminal <- send (ActorPollWith (actorId, incarnation))
  pure (terminal >>= decodeTerminal cell)
  where
    decodeTerminal retained status = case status of
      ActorCompletedStatus ->
        case readExitCell status retained of
          Just value -> Just (Completed value)
          Nothing -> error "Tidepool.Actor.pollExit: completed actor has an empty exit cell"
      ActorFailedStatus summary -> Just (Failed (ActorFailure summary))
      ActorCancelledStatus summary -> Just (Cancelled (CancelReason summary))
