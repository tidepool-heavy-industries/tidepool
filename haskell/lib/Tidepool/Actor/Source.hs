{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}

module Tidepool.Actor.Source
  ( Source
  , progressSource
  , settlementSource
  , installSource
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Kind (Type)
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
import Tidepool.Effects.Core (ActorKernel (..))

data Source (protocol :: Type -> Type) where
  ProgressSource
    :: Progress progress
    -> (ProgressState progress -> protocol ())
    -> Source protocol
  SettlementSource
    :: Response result
    -> (Either ResponseFailure (ResponseResult result) -> protocol ())
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

installSource :: Member ActorKernel effs => Source protocol -> Eff effs ()
installSource (ProgressSource (Progress (RequestId request)) project) =
  send (ActorInstallProgressSourceWith request (sourceEntry project))
installSource (SettlementSource response@(Response (RequestId request) _) project) =
  send (ActorInstallSettlementSourceWith request
    (sourceEntry (project . settledResponse response)))

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
