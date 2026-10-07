{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE RoleAnnotations #-}

-- | Engine-private representation of persistent-agent requests and replies.
module Tidepool.Agent.Reply.Internal
  ( RequestId (..)
  , Request (..)
  , RequestOptions (..)
  , defaultRequestOptions
  , RequestError (..)
  , SettlementReporting (..)
  , Reply (..)
  , RequestScope (..)
  , RequestScopeError (..)
  , currentRequest
  , currentRequestSited
  , requestReplyOf
  , requestIdNumber
  , Replies (..)
  , Progress (..)
  , ProgressSink (..)
  , ProgressCursor (..)
  , ProgressState (..)
  , PendingProgress (..)
  , reportRequestProgress
  , reportRequestProgressSited
  , pollProgress
  , pollProgressSited
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
  , responseActor
  , replyRequestId
  , readResponse
  , attemptReply
  , reply
  , pollResponse
  , cancelRequest
  , retainRequest
  , abandonResponse
  , forgetResponse
  , pollReply
  , attemptAcknowledgeCancellation
  , acknowledgeCancellation
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Tidepool.Internal.RequestSite (RequestSite)
import Data.Kind (Type)
import Data.Text (Text)
import qualified Data.Text as Text
import Data.Void (Void)
import Prelude
import Tidepool.Duration (Duration)
import Tidepool.Agent.Ref (AgentRef, agentAddressText)
import Tidepool.Inspection.Display (Display (..), WorkbenchDisplay (workbenchReplyDisplay), opaqueHandle)

import Tidepool.Internal.ExitCell
  ( ExitCell
  , fillExitCell
  , newExitCell
  , readExitCell
  )
import Tidepool.Effects.Core
  ( WorkerLifetime (..)
  , GitOid
  , SubmissionObservation
  , WorktreeError
  , WorktreeReceipt
  , AgentRosterState (..)
  , ProviderHealth (..)
  )

newtype RequestId = RequestId Int
  deriving (Show, Eq, Ord)


-- | One singular request-control identity and its retained typed result cell.
data Request result where
  Request :: RequestId -> AgentRef -> ExitCell pending (ResponseResult result) -> Request result

instance Show (Request result) where
  show (Request request actor _) =
    "Request { request = " <> show request <> ", actor = " <> show actor <> " }"

instance Display (Request result) where
  displayTree (Request (RequestId request) actor _) =
    opaqueHandle ("request " <> tshow request <> " to agent " <> agentAddressText actor)

data SettlementReporting = NotifyOwner | Silent
  deriving (Show, Eq)

data RequestOptions = RequestOptions
  { requestLabel :: Maybe Text
  , requestGuidance :: Maybe Text
  , requestDeadline :: Maybe Duration
  , requestReporting :: SettlementReporting
  , requestLifetime :: WorkerLifetime
  }
  deriving (Show, Eq)

defaultRequestOptions :: RequestOptions
defaultRequestOptions = RequestOptions Nothing Nothing Nothing NotifyOwner ActorOwned

data RequestError
  = RequestReservationRejected ReplyError
  | RequestSubmissionRejected ReplyError
  | RequestInvalidDeadline Text
  deriving (Show, Eq)

newtype Reply (result :: Type) = Reply RequestId
  deriving (Show, Eq)

data RequestScopeError
  = NoCurrentRequest
  | RequestTypeMismatch
  | RequestInputShadowed
  deriving (Show, Eq)

data RequestScope input (result :: Type)
  = RequestUnavailable RequestScopeError
  | RequestActive RequestId input

requestReplyOf :: RequestScope input result -> Maybe (Reply result)
requestReplyOf (RequestUnavailable _) = Nothing
requestReplyOf (RequestActive request _) = Just (Reply request)

requestIdNumber :: RequestId -> Int
requestIdNumber (RequestId request) = request

instance Display (Reply result) where
  displayTree (Reply (RequestId request)) = opaqueHandle ("reply for request " <> tshow request)

-- | Independent observers carry their own last-seen revision. Handles do not
-- contain a shared read position and observing never consumes an update.
type role Progress nominal
newtype Progress (progress :: Type) = Progress RequestId
  deriving (Show, Eq)

instance Display (Progress progress) where
  displayTree (Progress (RequestId request)) = opaqueHandle ("progress of request " <> tshow request)

-- | Publication authority is mounted only for the active request's target.
type role ProgressSink nominal
newtype ProgressSink (progress :: Type) = ProgressSink RequestId
  deriving (Show, Eq)

instance Display (ProgressSink progress) where
  displayTree (ProgressSink (RequestId request)) = opaqueHandle ("progress sink for request " <> tshow request)

newtype ProgressCursor = ProgressCursor Int
  deriving (Show, Eq, Ord)

data ProgressState progress
  = ProgressPending
  | ProgressUpdate ProgressCursor progress
  | ProgressClosed
  | ProgressRejected ReplyError
  deriving (Show, Eq)

-- | Everything the host already tracks about the producing actor of one
-- still-pending request or watch dependency, carried as data so a caller can
-- tell whether work is moving without polling again: the same lifecycle and
-- provider evidence 'Tidepool.Actors.Internal.Agent.observeAgent' reports,
-- not a second tracker. 'pendingWatched' is set when a registered watch will
-- wake the caller once this settles.
data PendingProgress = PendingProgress
  { pendingActorState :: AgentRosterState
  , pendingProviderHealth :: ProviderHealth
  , pendingLastActivityUnixMs :: Maybe Int
  , pendingProgressRevision :: Maybe Int
  , pendingWatched :: Bool
  }
  deriving (Show, Eq)

-- | An observation of presentation for one exact request, not a new assignment.
data RequestUpdate = RequestUpdate RequestId Int
  deriving (Show, Eq)

instance Display RequestUpdate where
  displayTree (RequestUpdate (RequestId request) update) =
    opaqueHandle ("update " <> tshow update <> " to request " <> tshow request)

tshow :: Int -> Text
tshow = Text.pack . show

data RequestUpdateState
  = UpdateQueued
  | UpdatePresented
  | UpdateTooLate
  | UpdateUnconfirmed Text
  | UpdateNotPresented Text
  deriving (Show, Eq)

updateRequest :: Member Replies effs => Request result -> Text -> Eff effs (Either ReplyError RequestUpdate)
updateRequest response message = do
  let request@(RequestId raw) = responseRequestId response
  result <- send (UpdateRequestWith raw message)
  pure (RequestUpdate request <$> result)

pollRequestUpdate :: Member Replies effs => RequestUpdate -> Eff effs (Either ReplyError RequestUpdateState)
pollRequestUpdate (RequestUpdate (RequestId request) sequence) =
  send (ObserveRequestUpdateWith request sequence)

data ReplyError
  = ReplyInvalidReadiness
  | ReplyStale
  | ReplyAlreadySettled
  | ReplyUnauthorized
  | ReplyWrongIncarnation
  | ReplyUpdatePending
  | ReplyProgressTypeMismatch
  | ReplySettlementCancelled
  deriving (Show, Eq)

data ResponseFailure
  = ResponseReleased
  | ResponseTargetUnavailable
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
  = -- | The target is presented with the request and has an observed
    -- provider turn underway. Distinct from 'ResponseStarting', which covers
    -- the admitted-but-not-yet-running interval. Carries the producing
    -- actor's own progress, so a re-poll has nothing to add that this did
    -- not already carry.
    ResponsePending PendingProgress
  | ResponseCancellationPending CancellationReason
  | ResponseReady (ResponseResult result)
  | ResponseUnavailable ResponseFailure
  | -- | The target is admitted for this request but has no started provider
    -- turn yet (queued, or presented but still idle). The 'Text' is a short
    -- human-readable detail, e.g. what the target is waiting on.
    ResponseStarting Text
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
  | ResponseForgetRejected ReplyError
  deriving (Show, Eq)

data ReplyState
  = ReplyOpen
  | ReplyCancellationRequested CancellationReason
  | ReplyClosed
  | ReplyObservationRejected ReplyError
  deriving (Show, Eq)

data RawResponseObservation
  = RawResponsePending PendingProgress
  | RawResponseCancellationPending CancellationReason
  | RawResponseReady
  | RawResponseUnavailable ResponseFailure
  | RawResponseRejected ReplyError
  | RawResponseStarting Text

data RawReplyObservation
  = RawReplyOpen
  | RawReplyCancellationRequested CancellationReason
  | RawReplyClosed
  | RawReplyRejected ReplyError

data Replies a where
  CurrentRequestWith :: RequestSite '[input, result, ResponseResult result] (RequestScope input result) -> Replies (RequestScope input result)
  ReserveRequestWith :: Maybe Text -> (Int, Int) -> Bool -> WorkerLifetime -> Replies (Either RequestError Int)
  SubmitRequestWith :: Int -> request -> (Int, Int) -> Maybe Duration -> Replies (Either RequestError ())
  -- | The 'Text' is a bounded, already-rendered preview of @result@ (see
  -- 'replyPreviewCharBudget'), carried alongside the live value so the
  -- settlement notice the reply produces can show readable text -- 'Text'
  -- fields included -- without the host forcing a packed byte array through
  -- a non-forcing heap walk. Rendering runs on the Haskell side, where the
  -- 'WorkbenchDisplay' instance a reply type already needs can read it.
  AttemptReplyWith :: Int -> result -> Text -> Replies (Either ReplyError Void)
  ReplyWith :: Int -> result -> Text -> Replies Void
  ObserveResponseWith :: Int -> Replies RawResponseObservation
  CancelRequestWith :: Int -> Replies CancelRequestOutcome
  RetainRequestWith :: Int -> WorkerLifetime -> Replies (Either ReplyError ())
  AbandonResponseWith :: Int -> Replies AbandonOutcome
  ForgetResponseWith :: Int -> Replies ForgetResponseOutcome
  ObserveReplyWith :: Int -> Replies RawReplyObservation
  AttemptAcknowledgeCancellationWith :: Int -> Replies (Either ReplyError Void)
  AcknowledgeCancellationWith :: Int -> Replies Void
  PublishProgressWith :: RequestSite '[progress] (Either ReplyError ()) -> progress -> Int -> Replies (Either ReplyError ())
  ObserveProgressWith :: RequestSite '[progress] (ProgressState progress) -> Int -> Replies (ProgressState progress)
  UpdateRequestWith :: Int -> Text -> Replies (Either ReplyError Int)
  ObserveRequestUpdateWith :: Int -> Int -> Replies (Either ReplyError RequestUpdateState)

{-# OPAQUE currentRequest #-}
currentRequest
  :: forall input result effs. Member Replies effs
  => Eff effs (RequestScope input result)
currentRequest = currentRequestSited (error "currentRequest: extractor must assign a typed site")

{-# OPAQUE currentRequestSited #-}
currentRequestSited
  :: forall input result effs. Member Replies effs
  => RequestSite '[input, result, ResponseResult result] (RequestScope input result) -> Eff effs (RequestScope input result)
currentRequestSited site = send (CurrentRequestWith site)

{-# OPAQUE reportRequestProgress #-}
reportRequestProgress
  :: forall progress effs. Member Replies effs
  => ProgressSink progress -> progress -> Eff effs (Either ReplyError ())
reportRequestProgress = reportRequestProgressSited (error "reportRequestProgress: extractor must assign a typed site")

{-# OPAQUE reportRequestProgressSited #-}
reportRequestProgressSited
  :: forall progress effs. Member Replies effs
  => RequestSite '[progress] (Either ReplyError ()) -> ProgressSink progress -> progress -> Eff effs (Either ReplyError ())
reportRequestProgressSited site (ProgressSink (RequestId request)) value =
  send (PublishProgressWith site value request)

{-# OPAQUE pollProgress #-}
pollProgress
  :: forall progress effs. Member Replies effs
  => Progress progress -> Eff effs (ProgressState progress)
pollProgress = pollProgressSited (error "pollProgress: extractor must assign a typed site")

{-# OPAQUE pollProgressSited #-}
pollProgressSited
  :: forall progress effs. Member Replies effs
  => RequestSite '[progress] (ProgressState progress) -> Progress progress -> Eff effs (ProgressState progress)
pollProgressSited site (Progress (RequestId request)) = send (ObserveProgressWith site request)

reserveRequest
  :: Member Replies effs
  => Maybe Text -> (Int, Int) -> SettlementReporting -> WorkerLifetime
  -> Eff effs (Either RequestError RequestId)
reserveRequest label target reporting lifetime =
  fmap (fmap RequestId) (send (ReserveRequestWith label target (reporting == NotifyOwner) lifetime))

submitRequest
  :: Member Replies effs
  => RequestId
  -> (Int, Int)
  -> request
  -> Maybe Duration
  -> Eff effs (Either RequestError ())
submitRequest (RequestId request) target requestPayload deadline =
  send (SubmitRequestWith request requestPayload target deadline)

newRequestHandles :: pending -> RequestId -> AgentRef -> (Request result, Reply result)
newRequestHandles pending request actor =
  (Request request actor (newExitCell pending), Reply request)

fillResponse :: Request result -> ResponseResult result -> ()
fillResponse (Request _ _ cell) = fillExitCell cell

responseRequestId :: Request result -> RequestId
responseRequestId (Request request _ _) = request

responseActor :: Request result -> AgentRef
responseActor (Request _ actor _) = actor

replyRequestId :: Reply result -> RequestId
replyRequestId (Reply request) = request

readResponse :: Request result -> Maybe (ResponseResult result)
readResponse (Request _ _ cell) = readExitCell () cell

-- | Character budget for the rendered reply 'reply' and 'attemptReply'
-- carry alongside the live value. Twice the settlement notice's byte budget
-- (the host's @SETTLEMENT_REPLY_PREVIEW_CHAR_BUDGET@, 8 KiB), so a rendering
-- this side cuts is always over the host's budget too: the host alone cuts,
-- at a line boundary, and names its budget when it does.
replyPreviewCharBudget :: Int
replyPreviewCharBudget = 16384

attemptReply
  :: (Member Replies effs, WorkbenchDisplay result)
  => Reply result
  -> result
  -> Eff effs (Either ReplyError Void)
attemptReply (Reply (RequestId request)) result =
  let (preview, _) = workbenchReplyDisplay replyPreviewCharBudget result
   in send (AttemptReplyWith request result preview)

reply :: (Member Replies effs, WorkbenchDisplay result) => Reply result -> result -> Eff effs Void
reply (Reply (RequestId request)) result =
  let (preview, _) = workbenchReplyDisplay replyPreviewCharBudget result
   in send (ReplyWith request result preview)

pollResponse
  :: Member Replies effs
  => Request result
  -> Eff effs (ResponseState result)
pollResponse response@(Request (RequestId request) _ _) = do
  observation <- send (ObserveResponseWith request)
  pure $ case observation of
    RawResponsePending progress -> ResponsePending progress
    RawResponseCancellationPending reason -> ResponseCancellationPending reason
    RawResponseReady ->
      case readResponse response of
        Just result -> ResponseReady result
        Nothing -> error "Tidepool response became ready before its Haskell cell was filled"
    RawResponseUnavailable failure -> ResponseUnavailable failure
    RawResponseRejected failure ->
      ResponseUnavailable (ResponseRejected failure)
    RawResponseStarting detail -> ResponseStarting detail

cancelRequest
  :: Member Replies effs
  => Request result
  -> Eff effs CancelRequestOutcome
cancelRequest (Request (RequestId request) _ _) = send (CancelRequestWith request)

-- | Transfer cleanup ownership without changing the target actor lifetime.
retainRequest
  :: Member Replies effs
  => Request result
  -> WorkerLifetime
  -> Eff effs (Either ReplyError ())
retainRequest (Request (RequestId request) _ _) lifetime = send (RetainRequestWith request lifetime)

abandonResponse
  :: Member Replies effs
  => Request result
  -> Eff effs AbandonOutcome
abandonResponse (Request (RequestId request) _ _) = send (AbandonResponseWith request)

forgetResponse
  :: Member Replies effs
  => Request result
  -> Eff effs ForgetResponseOutcome
forgetResponse (Request (RequestId request) _ _) = send (ForgetResponseWith request)

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
