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
  , onShutdown
  , Source
  , progressSource
  , settlementSource
  , withSources
  , stateful
  , EffectProfile (..)
  , LaunchRole (..)
  , ReadOnlyEffects
  , ReadWriteEffects
  , ShutdownReason (..)
  , startActor
  , beginActorForkGroup
  , startActorFork
  , commitActorForkGroup
  , abortActorForkGroup
  , runActor
  , call
  , cast
  , drainActor
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
import Tidepool.Actor.Source (Source, progressSource, settlementSource, installSource)

import Tidepool.Actor.Internal
  ( ActorDefinition
  , pattern ActorDefinition
  , label
  , effectProfile
  , initialization
  , behavior
  , onShutdown
  , actorLaunchWorktrees
  , actorSources
  , withSources
  , ActorRef (..)
  , EffectProfile (..)
  , ReadOnlyEffects
  , ReadWriteEffects
  , ShutdownReason (..)
  )
import Tidepool.Effects.Core
  ( Actor (..)
  , ActorLaunchRole (..)
  , ActorEffectProfile (..)
  , ActorKernel (..)
  , ActorLocal (..)
  , ActorTerminalStatus (..)
  )
import Tidepool.Internal.ExitCell
  ( fillExitCell
  , newExitCell
  , readExitCell
  )

data LaunchRole
  = RootRole
  | ResearchRole
  | CodingRole
  | ScaffoldingRole
  | IntegrationRole
  | InheritedRole
  deriving (Show, Eq)

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
-- This operation returns only after the child has installed its behavior and
-- reached readiness.
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
  , onShutdown = shutdownAction
  } startup = do
  let cell = newExitCell startup
      shutdownEntry reasonCode =
        raiseKernel (shutdownAction (decodeShutdownReason reasonCode))
      entry _ = do
        send (ActorInstallShutdownWith 0 shutdownEntry)
        initial <- raiseKernel (startupAction startup)
        mapM_ installSource (actorSources definition)
        send ActorReadyWith
        result <- raiseKernel (install startup initial)
        case fillExitCell cell result of
          () -> pure ()
  (actorId, incarnation, _) <- send
    (ActorStartWith actorLabel entry ActorInheritedRole (profileCode profile) (actorLaunchWorktrees definition))
  pure (ActorRef actorId incarnation cell)

-- | Trusted context-fork launch. The runtime snapshots the caller's lexical
-- environment and the host forks its provider conversation at the active
-- tool call. Normal authored Shoal code reaches this through `unfold`.
{-# NOINLINE startActorFork #-}
beginActorForkGroup
  :: Member Actor effs
  => Bool
  -> Text
  -> [Text]
  -> Eff effs (Int, Text, [Text])
beginActorForkGroup relative group branches =
  send (ActorBeginForkGroupWith relative group branches)

startActorFork
  :: forall effs startup api exit
  . Member Actor effs
  => LaunchRole
  -> Int
  -> ActorDefinition startup api exit
  -> startup
  -> Eff effs (ActorRef api exit, Text)
startActorFork launchRole forkGroup definition@ActorDefinition
  { label = actorLabel
  , effectProfile = profile
  , initialization = startupAction
  , behavior = install
  , onShutdown = shutdownAction
  } startup = do
  let cell = newExitCell startup
      shutdownEntry reasonCode =
        raiseKernel (shutdownAction (decodeShutdownReason reasonCode))
      entry _ = do
        send (ActorInstallShutdownWith 0 shutdownEntry)
        initial <- raiseKernel (startupAction startup)
        mapM_ installSource (actorSources definition)
        send ActorReadyWith
        result <- raiseKernel (install startup initial)
        case fillExitCell cell result of
          () -> pure ()
  (actorId, incarnation, allocatedPath) <- send
    (ActorForkWith actorLabel entry forkGroup (roleCode launchRole) (profileCode profile) (actorLaunchWorktrees definition))
  pure (ActorRef actorId incarnation cell, allocatedPath)

commitActorForkGroup :: Member Actor effs => Int -> Eff effs ()
commitActorForkGroup = send . ActorCommitForkGroupWith

abortActorForkGroup :: Member Actor effs => Int -> Eff effs ()
abortActorForkGroup = send . ActorAbortForkGroupWith

roleCode :: LaunchRole -> ActorLaunchRole
roleCode RootRole = ActorRootRole
roleCode ResearchRole = ActorResearchRole
roleCode CodingRole = ActorCodingRole
roleCode ScaffoldingRole = ActorScaffoldingRole
roleCode IntegrationRole = ActorIntegrationRole
roleCode InheritedRole = ActorInheritedRole

profileCode :: EffectProfile protocol effs -> ActorEffectProfile
profileCode ReadWrite = ActorReadWriteProfile
profileCode ReadOnly = ActorReadOnlyProfile

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

-- | Close an owned stateful actor's sources and mailbox and request completion
-- of accepted messages. Returns after admission closes; 'awaitExit' observes
-- the final state. A paused handler needs repair before draining.
drainActor
  :: Member Actor effs
  => ActorRef protocol state
  -> Eff effs ()
drainActor (ActorRef actorId incarnation _) =
  send (ActorDrainWith (actorId, incarnation))

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

-- | Define a mailbox actor with retained state. Each successful message commits
-- its reply and next state together; a failed handler pauses for replacement.
stateful
  :: Member (ActorLocal protocol) effs
  => Text
  -> EffectProfile protocol effs
  -> (forall result. state -> protocol result -> Eff effs (result, state))
  -> ActorDefinition state protocol state
stateful actorLabel profile step = ActorDefinition
  { label = actorLabel
  , effectProfile = profile
  , initialization = pure
  , behavior = \_ -> statefulLoop step
  , onShutdown = const (pure ())
  }

statefulLoop
  :: forall state protocol effs
   . Member (ActorLocal protocol) effs
  => (forall result. state -> protocol result -> Eff effs (result, state))
  -> state
  -> Eff effs state
statefulLoop step state = do
  send @(ActorLocal protocol) (ActorCheckpointWith 0 state)
  next <- receiveSited @(Maybe state) 0 (\message -> do
    (reply, nextState) <- step state message
    pure (reply, Just nextState))
  case next of
    Nothing -> pure state
    Just nextState -> statefulLoop step nextState

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
