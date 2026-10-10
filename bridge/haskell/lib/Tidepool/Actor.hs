{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE TypeFamilies #-}
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
-- closures and lazy structures; 'awaitExit' repeatedly observes the retained
-- runtime snapshot through the caller's compiler-issued type witness.
--
-- Target failure and cancellation are domain-visible lifecycle outcomes.
-- 'Unavailable' reports a failure to obtain the retained exit value, distinct
-- from failure of the actor's authored behavior.
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
  , ActorLifecycle (..)
  , lifecycleSource
  , withSources
  , stateful
  , EffectProfile (..)
  , ReadOnlyEffects
  , ReadWriteEffects
  , ShutdownReason (..)
  , startActor
  , startActorSited
  , startActorWithSite
  , startUnitActor
  , startUnitActorSited
  , runActor
  , runActorSited
  , call
  , cast
  , drainActor
  , replaceActor
  , replaceActorSited
  , replaceActorWithSite
  , receive
  , serve
  , ActorExit (..)
  , ActorFailure (..)
  , ActorObservationFailure (..)
  , CancelReason (..)
  , awaitExit
  , awaitExitSited
  , pollExit
  , pollExitSited
  ) where

import Control.Monad.Freer (Eff, Member, raise, send)
import Data.Text (Text)
import Tidepool.Internal.RequestSite (RequestSite, requestSiteIdentity)
import Prelude
import Tidepool.Actor.Source
  ( Source
  , progressSource
  , settlementSource
  , ActorLifecycle (..)
  , lifecycleSource
  , installSource
  )

import Tidepool.Actor.Internal
  ( ActorDefinition (..)
  , pattern ActorDefinition
  , label
  , effectProfile
  , initialization
  , behavior
  , onShutdown
  , actorWorkspace
  , actorSources
  , withSources
  , ActorRef (..)
  , EffectProfile (..)
  , profileCode
  , ReadOnlyEffects
  , ReadWriteEffects
  , ShutdownReason (..)
  )
import Tidepool.Effects.Core
  ( Actor (..)
  , ActorKernel (..)
  , ActorLocal (..)
  )
import Tidepool.Internal.ActorExit
  ( ActorExit (..), ActorFailure (..), CancelReason (..), ActorObservationFailure (..) )

-- | Start a fresh actor from an authored definition. Tidepool captures the
-- definition's exact Haskell environment internally; authored code does not
-- manage a separate deployment value or preparation step.
--
-- The actor inherits its caller's current workspace and access unless an
-- opaque workspace grant is explicitly attached to the definition.
-- This operation returns only after the child has installed its behavior and
-- reached readiness.
{-# OPAQUE startActor #-}
startActor
  :: forall exit startup api effs
   . Member Actor effs
  => ActorDefinition startup api exit
  -> startup
  -> Eff effs (ActorRef api exit)
startActor = startActorSited (error "startActor: extractor must assign a typed site")

{-# OPAQUE startActorSited #-}
startActorSited
  :: forall exit startup api effs. Member Actor effs
  => RequestSite '[exit] (ActorRef api exit)
  -> ActorDefinition startup api exit -> startup -> Eff effs (ActorRef api exit)
startActorSited = startActorWithSite

{-# OPAQUE startActorWithSite #-}
startActorWithSite
  :: forall exit startup api reply effs. Member Actor effs
  => RequestSite '[exit] reply
  -> ActorDefinition startup api exit -> startup -> Eff effs (ActorRef api exit)
startActorWithSite site definition@ActorDefinition
  { label = actorLabel
  , effectProfile = profile
  , initialization = startupAction
  , behavior = install
  , onShutdown = shutdownAction
  } startup = do
  let shutdownEntry reasonCode =
        raiseKernel (shutdownAction (decodeShutdownReason reasonCode))
      entry _ = do
        send (ActorInstallShutdownWith 0 shutdownEntry)
        initial <- raiseKernel (startupAction startup)
        mapM_ installSource (actorSources definition)
        send ActorReadyWith
        result <- raiseKernel (install startup initial)
        send (ActorPublishExitWith result site)
  (actorId, incarnation, _) <- send
    (ActorStartWith actorLabel site entry (profileCode profile) (actorWorkspace definition))
  pure (ActorRef actorId incarnation)

-- | Private unit-exit launch used by forwarding services whose protocol remains
-- polymorphic. Its evidence contains only the concrete terminal unit type.
{-# OPAQUE startUnitActor #-}
startUnitActor
  :: forall exit startup api effs. (exit ~ (), Member Actor effs)
  => ActorDefinition startup api exit -> startup -> Eff effs (ActorRef api exit)
startUnitActor = startUnitActorSited (error "startUnitActor: extractor must assign a typed site")

{-# OPAQUE startUnitActorSited #-}
startUnitActorSited
  :: forall exit startup api effs. (exit ~ (), Member Actor effs)
  => RequestSite '[exit] exit
  -> ActorDefinition startup api exit -> startup -> Eff effs (ActorRef api exit)
startUnitActorSited = startActorWithSite

-- | Replace a stateful handler using its committed state. The runtime retains
-- queued inputs and source positions; initialization is never rerun.
{-# OPAQUE replaceActor #-}
replaceActor
  :: forall state protocol effs. Member Actor effs
  => ActorRef protocol state
  -> ActorDefinition state protocol state
  -> Eff effs (ActorRef protocol state)
replaceActor = replaceActorSited (error "replaceActor: extractor must assign a typed site")

{-# OPAQUE replaceActorSited #-}
replaceActorSited
  :: forall state protocol effs. Member Actor effs
  => RequestSite '[state] (ActorRef protocol state)
  -> ActorRef protocol state -> ActorDefinition state protocol state -> Eff effs (ActorRef protocol state)
replaceActorSited = replaceActorWithSite

{-# OPAQUE replaceActorWithSite #-}
replaceActorWithSite
  :: forall state protocol reply effs. Member Actor effs
  => RequestSite '[state] reply
  -> ActorRef protocol state -> ActorDefinition state protocol state -> Eff effs (ActorRef protocol state)
replaceActorWithSite site (ActorRef previousId previousIncarnation) definition@ActorDefinitionInternal
  { internalLabel = actorLabel
  , internalEffectProfile = profile
  , internalOnShutdown = shutdownAction
  , internalReplacement = replacement
  } = case replacement of
    Nothing -> error "replaceActor requires a stateful definition"
    Just install -> do
      let shutdownEntry reasonCode =
            raiseKernel (shutdownAction (decodeShutdownReason reasonCode))
          entry committed = do
            send (ActorInstallShutdownWith 0 shutdownEntry)
            mapM_ installSource (actorSources definition)
            send ActorReadyWith
            result <- raiseKernel (install committed)
            send (ActorPublishExitWith result site)
      (actorId, incarnation) <- send (ActorReplaceWith
        (previousId, previousIncarnation) site entry actorLabel (profileCode profile))
      pure (ActorRef actorId incarnation)

decodeShutdownReason :: Int -> ShutdownReason
decodeShutdownReason 0 = ShutdownCompleted
decodeShutdownReason 1 = ShutdownFailed
decodeShutdownReason _ = ShutdownCancelled

raiseKernel :: Eff effs a -> Eff (ActorKernel ': effs) a
raiseKernel = raise

-- | Start a supervised one-shot actor and wait for its exact terminal value.
{-# OPAQUE runActor #-}
runActor
  :: forall exit startup api effs. Member Actor effs
  => ActorDefinition startup api exit -> startup -> Eff effs (ActorExit exit)
runActor = runActorSited (error "runActor: extractor must assign a typed site")

{-# OPAQUE runActorSited #-}
runActorSited
  :: forall exit startup api effs. Member Actor effs
  => RequestSite '[exit] (ActorExit exit)
  -> ActorDefinition startup api exit -> startup -> Eff effs (ActorExit exit)
runActorSited site definition startup = startActorWithSite site definition startup >>= awaitExitSited site

-- | Make one synchronous request to an exact actor incarnation. Runtime
-- lifecycle failure abandons this fragment; it is never fabricated as a
-- value of the protocol's result type.
call
  :: Member Actor effs
  => ActorRef protocol exit
  -> protocol result
  -> Eff effs result
call (ActorRef actorId incarnation) request =
  send (ActorCallWith (actorId, incarnation) request)

-- | Transfer one unit-result request into an exact actor mailbox. Returning
-- means the mailbox accepted ownership, not that the target handled it.
cast
  :: Member Actor effs
  => ActorRef protocol exit
  -> protocol ()
  -> Eff effs ()
cast (ActorRef actorId incarnation) request =
  send (ActorCastWith (actorId, incarnation) request)

-- | Close an owned stateful actor's sources and mailbox and request completion
-- of accepted messages. Returns after admission closes; 'awaitExit' observes
-- the final state. A paused handler needs repair before draining. Once a
-- drain has been requested, a handler failure ends the actor as 'Failed'
-- instead of pausing it: draining means "finish what you accepted, then
-- exit", and a caller who wants a failure to pause for replacement must
-- replace before draining.
drainActor
  :: Member Actor effs
  => ActorRef protocol state
  -> Eff effs ()
drainActor (ActorRef actorId incarnation) =
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
receive handler = receiveSited unreachableSiteId handler
  where
    unreachableSiteId = error "receive: extractor must assign a typed site"

-- Extractor substrate. The stable site key correlates the receive suspension
-- with its two private settlement steps and gives observability a source-level
-- identity without exposing a reply token.
{-# OPAQUE receiveSited #-}
receiveSited
  :: forall next protocol effs
   . Member (ActorLocal protocol) effs
  => RequestSite '[] next
  -> (forall result. protocol result -> Eff effs (result, next))
  -> Eff effs next
receiveSited site handler = send (ActorReceiveWith site kernelHandler)
  where
    kernelHandler :: forall result. protocol result -> Eff (ActorKernel ': effs) ()
    kernelHandler request = do
      (reply, next) <- raiseKernel (handler request)
      send (ActorReplyWith (requestSiteIdentity site) reply)
      send (ActorContinueWith (requestSiteIdentity site) next)

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
  => RequestSite '[] state
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
stateful actorLabel profile step = ActorDefinitionInternal
  { internalLabel = actorLabel
  , internalEffectProfile = profile
  , internalInitialization = pure
  , internalBehavior = \_ -> statefulLoop step
  , internalOnShutdown = const (pure ())
  , internalWorkspace = Nothing
  , internalSources = []
  , internalReplacement = Just (statefulLoop step)
  }

statefulLoop
  :: forall state protocol effs
   . Member (ActorLocal protocol) effs
  => (forall result. state -> protocol result -> Eff effs (result, state))
  -> state
  -> Eff effs state
statefulLoop step state = do
  send @(ActorLocal protocol) (ActorCheckpointWith 0 state)
  next <- send @(ActorLocal protocol) (ActorReceiveStatefulWith 0 kernelHandler)
  case next of
    Nothing -> pure state
    Just nextState -> statefulLoop step nextState
  where
    kernelHandler :: forall result. protocol result -> Eff (ActorKernel ': effs) ()
    kernelHandler message = do
      (reply, nextState) <- raiseKernel (step state message)
      send (ActorReplyWith 0 reply)
      send (ActorContinueWith 0 (Just nextState))

-- | Observe this exact actor incarnation's retained terminal result.
{-# OPAQUE awaitExit #-}
awaitExit :: forall exit api effs. Member Actor effs => ActorRef api exit -> Eff effs (ActorExit exit)
awaitExit = awaitExitSited (error "awaitExit: extractor must assign a typed site")

{-# OPAQUE awaitExitSited #-}
awaitExitSited :: forall exit api effs. Member Actor effs
  => RequestSite '[exit] (ActorExit exit) -> ActorRef api exit -> Eff effs (ActorExit exit)
awaitExitSited site (ActorRef actorId incarnation) = send (ActorWaitWith site (actorId, incarnation))

-- | Observe an exact actor without parking the caller.
{-# OPAQUE pollExit #-}
pollExit :: forall exit api effs. Member Actor effs => ActorRef api exit -> Eff effs (Maybe (ActorExit exit))
pollExit = pollExitSited (error "pollExit: extractor must assign a typed site")

{-# OPAQUE pollExitSited #-}
pollExitSited :: forall exit api effs. Member Actor effs
  => RequestSite '[exit] (Maybe (ActorExit exit)) -> ActorRef api exit -> Eff effs (Maybe (ActorExit exit))
pollExitSited site (ActorRef actorId incarnation) = send (ActorPollWith site (actorId, incarnation))
