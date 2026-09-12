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
  , RequestId (..)
  , Response (..)
  , ResponseFailure
  , ResponseResult
  , readResponse
  )
import Tidepool.Effects.Core (ActorKernel (..), CommandResult)
import Tidepool.Command.Types (Job (..))
import Tidepool.Internal.ActorRef (ActorRef (..))

-- | Runtime lifecycle facts. Live does not imply application readiness;
-- completion does not assert resource cleanup or acceptance of delivered work.
data ActorLifecycle
  = ActorLive
  | ActorPaused Text
  | ActorFinished Text
  | ActorFailed Text
  | ActorCancelled Text
  deriving (Eq, Show)

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

installSource :: Member ActorKernel effs => Source protocol -> Eff effs ()
installSource (CommandSource (Job job) project) =
  send (ActorInstallCommandSourceWith job (sourceEntry project))
installSource (ProgressSource (Progress (RequestId request)) project) =
  send (ActorInstallProgressSourceWith request (sourceEntry project))
installSource (SettlementSource response@(Response (RequestId request) _ _ _) project) =
  send (ActorInstallSettlementSourceWith request
    (sourceEntry (project . settledResponse response)))
installSource (LifecycleSource (ActorRef actor incarnation _) project) =
  send (ActorInstallLifecycleSourceWith (actor, incarnation) (sourceEntry project))

sourceEntry :: (event -> protocol ()) -> Int -> Eff '[ActorKernel] ()
sourceEntry project _ = do
  event <- send ActorSourceInputWith
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
