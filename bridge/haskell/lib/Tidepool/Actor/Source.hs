{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}

module Tidepool.Actor.Source
  ( Source
  , progressSource
  , settlementSource
  , ActorLifecycle (..)
  , commandSource
  , lifecycleSource
  , installSource
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Kind (Type)
import Data.Text (Text)
import Tidepool.Agent.Reply.Internal
  ( Progress (..)
  , ProgressState
  , RawResponseObservation (..)
  , Replies (..)
  , RequestId (..)
  , Response (..)
  , ResponseFailure
  , ResponseResult
  , readResponse
  )
import Tidepool.Effects.Core (ActorKernel (..), ActorLifecycle (..), CommandResult)
import Tidepool.Command.Types (Job (..))
import Tidepool.Internal.ActorRef (ActorRef (..))

data Source (protocol :: Type -> Type) where
  CommandSource :: Job -> (CommandResult -> protocol ()) -> Source protocol
  ProgressSource
    :: Progress progress
    -> (ProgressState progress -> protocol ())
    -> Source protocol
  SettlementSource
    :: Response result
    -> (Either ResponseFailure (ResponseResult result) -> protocol ())
    -> Source protocol
  LifecycleSource
    :: ActorRef api exit
    -> (ActorLifecycle -> protocol ())
    -> Source protocol

-- | Capture current progress and every subsequent publication, including closure.
progressSource
  :: Progress progress
  -> (ProgressState progress -> protocol ())
  -> Source protocol
progressSource = ProgressSource

settlementSource
  :: Response result
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
installSource :: Member ActorKernel effs => Source protocol -> Eff effs ()
installSource (CommandSource (Job job) project) =
  send (ActorInstallCommandSourceWith job (sourceEntry ActorCommandInputWith project))
installSource (ProgressSource (Progress (RequestId request)) project) =
  send (ActorInstallProgressSourceWith request
    (sourceEntry (ObserveProgressWith request) project))
installSource (SettlementSource response@(Response (RequestId request) _ _ _) project) =
  send (ActorInstallSettlementSourceWith request
    (sourceEntry (ObserveResponseWith request) (project . settledResponse response)))
installSource (LifecycleSource (ActorRef actor incarnation _) project) =
  send (ActorInstallLifecycleSourceWith (actor, incarnation)
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
  :: Response result
  -> RawResponseObservation
  -> Either ResponseFailure (ResponseResult result)
settledResponse response RawResponseReady = case readResponse response of
  Just result -> Right result
  Nothing -> error "settlement source has no filled response cell"
settledResponse _ (RawResponseUnavailable failure) = Left failure
settledResponse _ _ = error "settlement source received a nonterminal observation"
