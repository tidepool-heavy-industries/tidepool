{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

module Tidepool.Actor.Source
  ( Source
  , progressSource
  , progressSourceSited
  , settlementSource
  , settlementSourceSited
  , ActorLifecycle (..)
  , commandSource
  , lifecycleSource
  , installSource
  , attachSource
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Kind (Type)
import Tidepool.Internal.RequestSite (RequestSite)
import Data.Text (Text)
import Tidepool.Agent.Reply.Internal
  ( Progress (..)
  , ProgressState
  , ResponseState (..)
  , Replies (..)
  , RequestId (..)
  , Request (..)
  , ResponseFailure
  , ResponseResult
  )
import Tidepool.Effects.Core (ActorKernel (..), ActorLocal (..), ActorLifecycle (..), CommandResult)
import Tidepool.Command.Types (Job (..))
import Tidepool.Internal.ActorRef (ActorRef (..))

data Source (protocol :: Type -> Type) where
  CommandSource :: Job -> (CommandResult -> protocol ()) -> Source protocol
  ProgressSource
    :: RequestSite '[progress] (ProgressState progress) -> Progress progress
    -> (ProgressState progress -> protocol ())
    -> Source protocol
  SettlementSource
    :: RequestSite '[ResponseResult result] (ResponseState result) -> Request result
    -> (Either ResponseFailure (ResponseResult result) -> protocol ())
    -> Source protocol
  LifecycleSource
    :: ActorRef api exit
    -> (ActorLifecycle -> protocol ())
    -> Source protocol

-- | Capture current progress and every subsequent publication, including closure.
{-# OPAQUE progressSource #-}
progressSource
  :: forall progress protocol. Progress progress
  -> (ProgressState progress -> protocol ())
  -> Source protocol
progressSource = progressSourceSited (error "progressSource: extractor must assign a typed site")

{-# OPAQUE progressSourceSited #-}
progressSourceSited
  :: forall progress protocol. RequestSite '[progress] (ProgressState progress) -> Progress progress
  -> (ProgressState progress -> protocol ())
  -> Source protocol
progressSourceSited = ProgressSource

{-# OPAQUE settlementSource #-}
settlementSource
  :: forall result protocol. Request result
  -> (Either ResponseFailure (ResponseResult result) -> protocol ())
  -> Source protocol
settlementSource = settlementSourceSited (error "settlementSource: extractor must assign a typed site")

{-# OPAQUE settlementSourceSited #-}
settlementSourceSited
  :: forall result protocol. RequestSite '[ResponseResult result] (ResponseState result)
  -> Request result
  -> (Either ResponseFailure (ResponseResult result) -> protocol ())
  -> Source protocol
settlementSourceSited = SettlementSource

lifecycleSource
  :: ActorRef api exit
  -> (ActorLifecycle -> protocol ())
  -> Source protocol
lifecycleSource = LifecycleSource

commandSource :: Job -> (CommandResult -> protocol ()) -> Source protocol
commandSource = CommandSource

-- Each source kind's entry asks for its delivered input with a request whose
-- reply type is closed: the runtime answers a host-built value only against
-- the reply type the request constructor declares, so a shared entry over a
-- bare @event@ variable could never be answered. Progress and settlement
-- entries use typed result observations through the ordinary 'Replies'
-- observation verbs; the runtime answers those with the delivered event
-- rather than with a live poll.
{-# OPAQUE installSource #-}
installSource :: Member ActorKernel effs => Source protocol -> Eff effs ()
installSource (CommandSource (Job job) project) =
  send (ActorInstallCommandSourceWith job (sourceEntry ActorCommandInputWith project))
installSource (ProgressSource site (Progress (RequestId request)) project) =
  send (ActorInstallProgressSourceWith request
    (sourceEntry (ObserveProgressWith site request) project))
installSource (SettlementSource site (Request (RequestId request) _) project) =
  send (ActorInstallSettlementSourceWith request
    (sourceEntry (ObserveResponseWith site request) (project . settledResponse)))
installSource (LifecycleSource (ActorRef actor incarnation) project) =
  send (ActorInstallLifecycleSourceWith (actor, incarnation)
    (sourceEntry ActorLifecycleInputWith project))

{-# OPAQUE attachSource #-}
attachSource
  :: forall protocol effs. Member (ActorLocal protocol) effs
  => (Int, Int) -> Source protocol -> Eff effs (Either Text ())
attachSource owner (CommandSource (Job job) project) =
  send @(ActorLocal protocol) (ActorLocalAttachCommandSourceWith (owner, job) (sourceEntry ActorCommandInputWith project))
attachSource owner (ProgressSource site (Progress (RequestId request)) project) =
  send @(ActorLocal protocol) (ActorLocalAttachProgressSourceWith (owner, request)
    (sourceEntry (ObserveProgressWith site request) project))
attachSource owner (SettlementSource site (Request (RequestId request) _) project) =
  send @(ActorLocal protocol) (ActorLocalAttachSettlementSourceWith (owner, request)
    (sourceEntry (ObserveResponseWith site request) (project . settledResponse)))
attachSource owner (LifecycleSource (ActorRef actor incarnation) project) =
  send @(ActorLocal protocol) (ActorLocalAttachLifecycleSourceWith (owner, (actor, incarnation))
    (sourceEntry ActorLifecycleInputWith project))

sourceEntry
  :: Member input '[ActorKernel, Replies]
  => input event
  -> (event -> protocol ())
  -> Int
  -> Eff '[ActorKernel, Replies] ()
sourceEntry input project _ = do
  event <- send input
  send (ActorReplyWith 0 (project event))

settledResponse :: ResponseState result -> Either ResponseFailure (ResponseResult result)
settledResponse (ResponseReady result) = Right result
settledResponse (ResponseUnavailable failure) = Left failure
settledResponse _ = error "settlement source received a nonterminal observation"
