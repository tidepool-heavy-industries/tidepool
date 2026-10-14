{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE PatternSynonyms #-}

-- | Typed request observation and one-shot target settlement.
module Tidepool.Agent.Reply
  ( RequestId
  , Request
  , RequestOptions (..)
  , defaultRequestOptions
  , RequestError (..)
  , SettlementReporting (..)
  , Reply
  , RequestScope (RequestUnavailable, RequestActive)
  , RequestScopeError (..)
  , currentRequest
  , requestReplyOf
  , replyRequestId
  , requestIdNumber
  , Replies
  , Progress
  , ProgressCursor (..)
  , ProgressState (..)
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
  , requestId
  , responseActor
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

import Tidepool.Agent.Reply.Internal
  ( Reply
  , RequestScope (RequestUnavailable)
  , RequestScopeError (..)
  , currentRequest
  , requestReplyOf
  , replyRequestId
  , requestIdNumber
  , Progress
  , ProgressCursor (..)
  , ProgressState (..)
  , pollProgress
  , Replies
  , RequestUpdate
  , RequestUpdateState (..)
  , updateRequest
  , pollRequestUpdate
  , ReplyError (..)
  , RequestId
  , Request
  , RequestOptions (..)
  , defaultRequestOptions
  , RequestError (..)
  , SettlementReporting (..)
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
  , attemptReply
  , pollResponse
  , cancelRequest
  , retainRequest
  , abandonResponse
  , forgetResponse
  , pollReply
  , attemptAcknowledgeCancellation
  , acknowledgeCancellation
  , reply
  , responseRequestId
  , responseActor
  )
import qualified Tidepool.Agent.Reply.Internal as Internal

-- | Observe the runtime-issued reply capability and request input. The pattern
-- cannot issue a scope or reconstruct a reply capability from a request ID.
pattern RequestActive :: Reply result -> input -> RequestScope input result
pattern RequestActive target input <- Internal.RequestScopeActive target input
{-# COMPLETE RequestUnavailable, RequestActive #-}

requestId :: Request result -> RequestId
requestId = responseRequestId
