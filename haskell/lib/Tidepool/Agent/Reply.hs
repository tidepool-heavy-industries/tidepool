{-# LANGUAGE FlexibleContexts #-}

-- | Typed request observation and one-shot target settlement.
module Tidepool.Agent.Reply
  ( RequestId
  , Response
  , Reply
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
  , responseLaunch
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

import Tidepool.Agent.Reply.Internal
  ( Reply
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
  , Response
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
  , abandonResponse
  , forgetResponse
  , pollReply
  , attemptAcknowledgeCancellation
  , acknowledgeCancellation
  , reply
  , responseRequestId
  , responseActor
  , responseLaunch
  )

requestId :: Response result -> RequestId
requestId = responseRequestId
