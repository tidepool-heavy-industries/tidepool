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
  , RawResponseObservation (..)
  , Replies (..)
  , RequestId (..)
  , Request (..)
  , ResponseFailure
  , ResponseResult
  , readResponse
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
    :: Request result
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

settlementSource
  :: Request result
  -> (Either ResponseFailure (ResponseResult result) -> protocol ())
  -> Source protocol
settlementSource = SettlementSource

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
-- entries read their request cells through the ordinary 'Replies'
-- observation verbs; the runtime answers those with the delivered event
-- rather than with a live poll.
{-# OPAQUE installSource #-}
installSource :: Member ActorKernel effs => Source protocol -> Eff effs ()
installSource (CommandSource (Job job) project) =
  send (ActorInstallCommandSourceWith job (sourceEntry ActorCommandInputWith project))
installSource (ProgressSource site (Progress (RequestId request)) project) =
  send (ActorInstallProgressSourceWith request
    (sourceEntry (ObserveProgressWith site request) project))
installSource (SettlementSource response@(Request (RequestId request) _ _) project) =
  send (ActorInstallSettlementSourceWith request
    (sourceEntry (ObserveResponseWith request) (project . settledResponse response)))
installSource (LifecycleSource (ActorRef actor incarnation _) project) =
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
attachSource owner (SettlementSource response@(Request (RequestId request) _ _) project) =
  send @(ActorLocal protocol) (ActorLocalAttachSettlementSourceWith (owner, request)
    (sourceEntry (ObserveResponseWith request) (project . settledResponse response)))
attachSource owner (LifecycleSource (ActorRef actor incarnation _) project) =
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

settledResponse
  :: Request result
  -> RawResponseObservation
  -> Either ResponseFailure (ResponseResult result)
settledResponse response RawResponseReady = case readResponse response of
  Just result -> Right result
  Nothing -> error "settlement source has no filled response cell"
settledResponse _ (RawResponseUnavailable failure) = Left failure
settledResponse _ _ = error "settlement source received a nonterminal observation"
