{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Engine-private representation of persistent-agent requests and replies.
module Tidepool.Agent.Reply.Internal
  ( RequestId (..)
  , RequestLabel (..)
  , RequestLabelError (..)
  , requestLabel
  , Response (..)
  , Reply (..)
  , Replies (..)
  , Progress (..)
  , ProgressSink (..)
  , ProgressCursor (..)
  , ProgressState (..)
  , reportRequestProgress
  , pollProgress
  , RequestUpdate
  , RequestUpdateState (..)
  , updateRequest
  , pollRequestUpdate
  , ReplyError (..)
  , ResponseFailure (..)
  , ResponseResult (..)
  , ExecutionReceipt (..)
  , WorktreeEvidence (..)
  , ResponseState (..)
  , CancellationReason (..)
  , CancelRequestOutcome (..)
  , AbandonOutcome (..)
  , ForgetResponseOutcome (..)
  , ReplyState (..)
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
  , cancelRequest
  , abandonResponse
  , forgetResponse
  , pollReply
  , attemptAcknowledgeCancellation
  , acknowledgeCancellation
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Char (isAsciiLower, isDigit)
import Data.Kind (Type)
import Data.String (IsString (fromString))
import Data.Text (Text)
import qualified Data.Text as Text
import Data.Void (Void)
import Prelude
import Tidepool.Duration (RequestDeadline)

import Tidepool.Internal.ExitCell
  ( ExitCell
  , fillExitCell
  , newExitCell
  , readExitCell
  )
import Tidepool.Effects.Core (GitOid, SubmissionObservation, WorktreeError, WorktreeReceipt)

newtype RequestId = RequestId Int
  deriving (Show, Eq, Ord)

newtype RequestLabel = RequestLabel Text
  deriving (Show, Eq, Ord)

data RequestLabelError
  = EmptyRequestLabel
  | InvalidRequestLabel Text
  | RequestLabelTooLong Text
  deriving (Show, Eq)

instance IsString RequestLabel where
  fromString = either (error . show) id . requestLabel . Text.pack

requestLabel :: Text -> Either RequestLabelError RequestLabel
requestLabel value
  | Text.null value = Left EmptyRequestLabel
  | Text.length value > 48 = Left (RequestLabelTooLong value)
  | Text.head value == '-' || Text.last value == '-' = Left (InvalidRequestLabel value)
  | "--" `Text.isInfixOf` value = Left (InvalidRequestLabel value)
  | Text.all valid value = Right (RequestLabel value)
  | otherwise = Left (InvalidRequestLabel value)
  where
    valid character = isAsciiLower character || isDigit character || character == '-'

data Response result where
  Response :: RequestId -> ExitCell pending (ResponseResult result) -> Response result

instance Show (Response result) where
  show (Response request _) = "Response " <> show request

newtype Reply result = Reply RequestId
  deriving (Show, Eq)

-- | Independent observers carry their own last-seen revision. Handles do not
-- contain a shared read position and observing never consumes an update.
newtype Progress (progress :: Type) = Progress RequestId
  deriving (Show, Eq)

-- | Publication authority is mounted only for the active request's target.
newtype ProgressSink (progress :: Type) = ProgressSink RequestId
  deriving (Show, Eq)

newtype ProgressCursor = ProgressCursor Int
  deriving (Show, Eq, Ord)

data ProgressState progress
  = ProgressPending
  | ProgressUpdate ProgressCursor progress
  | ProgressClosed
  | ProgressRejected ReplyError
  deriving (Show, Eq)

-- | An observation of presentation for one exact request, not a new assignment.
data RequestUpdate = RequestUpdate RequestId Int
  deriving (Show, Eq)

data RequestUpdateState
  = UpdateQueued
  | UpdatePresented
  | UpdateTooLate
  | UpdateUnconfirmed Text
  | UpdateNotPresented Text
  deriving (Show, Eq)

updateRequest :: Member Replies effs => Response result -> Text -> Eff effs (Either ReplyError RequestUpdate)
updateRequest response message = do
  let request@(RequestId raw) = responseRequestId response
  result <- send (UpdateRequestWith raw message)
  pure (RequestUpdate request <$> result)

pollRequestUpdate :: Member Replies effs => RequestUpdate -> Eff effs (Either ReplyError RequestUpdateState)
pollRequestUpdate (RequestUpdate (RequestId request) sequence) =
  send (ObserveRequestUpdateWith request sequence)

data ReplyError
  = ReplyStale
  | ReplyAlreadySettled
  | ReplyUnauthorized
  | ReplyWrongIncarnation
  | ReplyUpdatePending
  | ReplySettlementCancelled
  deriving (Show, Eq)

data ResponseFailure
  = ResponseTargetUnavailable
  | ResponseTargetFailed Text
  | ResponseTargetCancelled Text
  | ResponseRequesterStopped
  | ResponseAbandoned
  | ResponseCancelled
  | ResponseDeadlineExceeded
  | ResponseSettlementFailed Text
  | ResponseRejected ReplyError
  deriving (Show, Eq)

data WorktreeEvidence
  = NoBoundWorktree
  | WorktreeObserved WorktreeReceipt GitOid SubmissionObservation
  | WorktreeObservationFailed WorktreeError
  deriving (Show, Eq)

data ExecutionReceipt = ExecutionReceipt
  { executionRequest :: RequestId
  , executionActorId :: Int
  , executionActorIncarnation :: Int
  }
  deriving (Show, Eq)

data ResponseResult result = ResponseResult
  { responseValue :: result
  , responseExecution :: ExecutionReceipt
  , responseWorktree :: WorktreeEvidence
  }
  deriving (Show, Eq)

data ResponseState result
  = ResponsePending
  | ResponseCancellationPending CancellationReason
  | ResponseReady (ResponseResult result)
  | ResponseUnavailable ResponseFailure
  deriving (Show, Eq)

data CancellationReason
  = CancelledByRequester
  | DeadlineExpired
  deriving (Show, Eq)

data CancelRequestOutcome
  = CancellationRequested
  | CancellationAlreadyRequested
  | CancellationAlreadyTerminal
  | CancellationRejected ReplyError
  deriving (Show, Eq)

data AbandonOutcome
  = ResponseAbandonedNow
  | ResponseAlreadyAbandoned
  | ResponseAlreadyTerminal
  | ResponseAbandonRejected ReplyError
  deriving (Show, Eq)

data ForgetResponseOutcome
  = ResponseForgotten
  | ResponseForgetPending
  | ResponseForgetTargetActive
  | ResponseRetainedByWatches [Int]
  | ResponseForgetRejected ReplyError
  deriving (Show, Eq)

data ReplyState
  = ReplyOpen
  | ReplyCancellationRequested CancellationReason
  | ReplyClosed
  | ReplyObservationRejected ReplyError
  deriving (Show, Eq)

data RawResponseObservation
  = RawResponsePending
  | RawResponseCancellationPending CancellationReason
  | RawResponseReady
  | RawResponseUnavailable ResponseFailure
  | RawResponseRejected ReplyError

data RawReplyObservation
  = RawReplyOpen
  | RawReplyCancellationRequested CancellationReason
  | RawReplyClosed
  | RawReplyRejected ReplyError

data Replies a where
  ReserveRequestWith :: Text -> (Int, Int) -> Replies Int
  SubmitRequestWith :: Int -> request -> (Int, Int) -> Maybe RequestDeadline -> Replies ()
  AttemptReplyWith :: Int -> result -> Replies (Either ReplyError Void)
  ReplyWith :: Int -> result -> Replies Void
  ObserveResponseWith :: Int -> Replies RawResponseObservation
  CancelRequestWith :: Int -> Replies CancelRequestOutcome
  AbandonResponseWith :: Int -> Replies AbandonOutcome
  ForgetResponseWith :: Int -> Replies ForgetResponseOutcome
  ObserveReplyWith :: Int -> Replies RawReplyObservation
  AttemptAcknowledgeCancellationWith :: Int -> Replies (Either ReplyError Void)
  AcknowledgeCancellationWith :: Int -> Replies Void
  PublishProgressWith :: Int -> progress -> Replies (Either ReplyError ())
  ObserveProgressWith :: Int -> Replies (ProgressState progress)
  UpdateRequestWith :: Int -> Text -> Replies (Either ReplyError Int)
  ObserveRequestUpdateWith :: Int -> Int -> Replies (Either ReplyError RequestUpdateState)

reportRequestProgress
  :: Member Replies effs
  => ProgressSink progress
  -> progress
  -> Eff effs (Either ReplyError ())
reportRequestProgress (ProgressSink (RequestId request)) value =
  send (PublishProgressWith request value)

pollProgress
  :: Member Replies effs
  => Progress progress
  -> Eff effs (ProgressState progress)
pollProgress (Progress (RequestId request)) = send (ObserveProgressWith request)

reserveRequest :: Member Replies effs => RequestLabel -> (Int, Int) -> Eff effs RequestId
reserveRequest (RequestLabel label) target = RequestId <$> send (ReserveRequestWith label target)

submitRequest
  :: Member Replies effs
  => RequestId
  -> (Int, Int)
  -> request
  -> Maybe RequestDeadline
  -> Eff effs ()
submitRequest (RequestId request) target requestPayload deadline =
  send (SubmitRequestWith request requestPayload target deadline)

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
    RawResponseCancellationPending reason -> ResponseCancellationPending reason
    RawResponseReady ->
      case readResponse response of
        Just result -> ResponseReady result
        Nothing -> error "Tidepool response became ready before its Haskell cell was filled"
    RawResponseUnavailable failure -> ResponseUnavailable failure
    RawResponseRejected failure ->
      ResponseUnavailable (ResponseRejected failure)

cancelRequest
  :: Member Replies effs
  => Response result
  -> Eff effs CancelRequestOutcome
cancelRequest (Response (RequestId request) _) = send (CancelRequestWith request)

abandonResponse
  :: Member Replies effs
  => Response result
  -> Eff effs AbandonOutcome
abandonResponse (Response (RequestId request) _) = send (AbandonResponseWith request)

forgetResponse
  :: Member Replies effs
  => Response result
  -> Eff effs ForgetResponseOutcome
forgetResponse (Response (RequestId request) _) = send (ForgetResponseWith request)

pollReply :: Member Replies effs => Reply result -> Eff effs ReplyState
pollReply (Reply (RequestId request)) = do
  observation <- send (ObserveReplyWith request)
  pure $ case observation of
    RawReplyOpen -> ReplyOpen
    RawReplyCancellationRequested reason -> ReplyCancellationRequested reason
    RawReplyClosed -> ReplyClosed
    RawReplyRejected failure -> ReplyObservationRejected failure

attemptAcknowledgeCancellation
  :: Member Replies effs
  => Reply result
  -> Eff effs (Either ReplyError Void)
attemptAcknowledgeCancellation (Reply (RequestId request)) =
  send (AttemptAcknowledgeCancellationWith request)

acknowledgeCancellation :: Member Replies effs => Reply result -> Eff effs Void
acknowledgeCancellation (Reply (RequestId request)) =
  send (AcknowledgeCancellationWith request)
