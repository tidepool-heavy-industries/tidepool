{-# LANGUAGE FlexibleContexts #-}

-- | Typed request observation and one-shot target settlement.
module Tidepool.Agent.Reply
  ( RequestId
  , Request
  , RequestOptions (..)
  , defaultRequestOptions
  , RequestError (..)
  , SettlementReporting (..)
  , Reply
  , RequestScope (..)
  , RequestScopeError (..)
  , currentRequest
  , requestReplyOf
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
  , RequestScope (..)
  , RequestScopeError (..)
  , currentRequest
  , requestReplyOf
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

requestId :: Request result -> RequestId
requestId = responseRequestId
