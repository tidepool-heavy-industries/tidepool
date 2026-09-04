{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}

-- | Engine-private representation of persistent-agent requests and replies.
module Tidepool.Agent.Reply.Internal
  ( RequestId (..)
  , Response (..)
  , Reply (..)
  , Replies (..)
  , ReplyError (..)
  , ResponseFailure (..)
  , ResponseResult (..)
  , WorktreeEvidence (..)
  , ResponseState (..)
  , RawResponseObservation (..)
  , reserveRequest
  , submitRequest
  , newRequestHandles
  , fillResponse
  , responseRequestId
  , replyRequestId
  , readResponse
  , attemptReply
  , reply
  , pollResponse
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Data.Void (Void)
import Prelude

import Tidepool.Internal.ExitCell
  ( ExitCell
  , fillExitCell
  , newExitCell
  , readExitCell
  )
import Tidepool.Effects.Core (GitOid, SubmissionObservation, WorktreeError)

newtype RequestId = RequestId Int
  deriving (Show, Eq, Ord)

data Response result where
  Response :: RequestId -> ExitCell pending (ResponseResult result) -> Response result

instance Show (Response result) where
  show (Response request _) = "Response " <> show request

newtype Reply result = Reply RequestId
  deriving (Show, Eq)

data ReplyError
  = ReplyStale
  | ReplyAlreadySettled
  | ReplyUnauthorized
  | ReplyWrongIncarnation
  deriving (Show, Eq)

data ResponseFailure
  = ResponseTargetUnavailable
  | ResponseTargetFailed Text
  | ResponseTargetCancelled Text
  | ResponseRequesterStopped
  | ResponseCancelled
  | ResponseDeadlineExceeded
  | ResponseRejected ReplyError
  deriving (Show, Eq)

data WorktreeEvidence
  = NoBoundWorktree
  | WorktreeObserved GitOid SubmissionObservation
  | WorktreeObservationFailed WorktreeError
  deriving (Show, Eq)

data ResponseResult result = ResponseResult
  { responseValue :: result
  , responseWorktree :: WorktreeEvidence
  }
  deriving (Show, Eq)

data ResponseState result
  = ResponsePending
  | ResponseReady (ResponseResult result)
  | ResponseUnavailable ResponseFailure
  deriving (Show, Eq)

data RawResponseObservation
  = RawResponsePending
  | RawResponseReady
  | RawResponseUnavailable ResponseFailure
  | RawResponseRejected ReplyError

data Replies a where
  ReserveRequestWith :: (Int, Int) -> Replies Int
  SubmitRequestWith :: Int -> request -> (Int, Int) -> Replies ()
  AttemptReplyWith :: Int -> result -> Replies (Either ReplyError Void)
  ReplyWith :: Int -> result -> Replies Void
  ObserveResponseWith :: Int -> Replies RawResponseObservation

reserveRequest :: Member Replies effs => (Int, Int) -> Eff effs RequestId
reserveRequest target = RequestId <$> send (ReserveRequestWith target)

submitRequest
  :: Member Replies effs
  => RequestId
  -> (Int, Int)
  -> request
  -> Eff effs ()
submitRequest (RequestId request) target requestPayload =
  send (SubmitRequestWith request requestPayload target)

newRequestHandles :: pending -> RequestId -> (Response result, Reply result)
newRequestHandles pending request =
  (Response request (newExitCell pending), Reply request)

fillResponse :: Response result -> ResponseResult result -> ()
fillResponse (Response _ cell) = fillExitCell cell

responseRequestId :: Response result -> RequestId
responseRequestId (Response request _) = request

replyRequestId :: Reply result -> RequestId
replyRequestId (Reply request) = request

readResponse :: Response result -> Maybe (ResponseResult result)
readResponse (Response _ cell) = readExitCell () cell

attemptReply
  :: Member Replies effs
  => Reply result
  -> result
  -> Eff effs (Either ReplyError Void)
attemptReply (Reply (RequestId request)) result =
  send (AttemptReplyWith request result)

reply :: Member Replies effs => Reply result -> result -> Eff effs Void
reply (Reply (RequestId request)) result = send (ReplyWith request result)

pollResponse
  :: Member Replies effs
  => Response result
  -> Eff effs (ResponseState result)
pollResponse response@(Response (RequestId request) _) = do
  observation <- send (ObserveResponseWith request)
  pure $ case observation of
    RawResponsePending -> ResponsePending
    RawResponseReady ->
      case readResponse response of
        Just result -> ResponseReady result
        Nothing -> error "Tidepool response became ready before its Haskell cell was filled"
    RawResponseUnavailable failure -> ResponseUnavailable failure
    RawResponseRejected failure ->
      ResponseUnavailable (ResponseRejected failure)
