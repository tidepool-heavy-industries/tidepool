{-# LANGUAGE FlexibleContexts #-}

-- | Typed request observation and one-shot target settlement.
module Tidepool.Agent.Reply
  ( RequestId
  , RequestLabel
  , RequestLabelError (..)
  , requestLabel
  , Response
  , Reply
  , Replies
  , ReplyError (..)
  , ResponseFailure (..)
  , ResponseResult (..)
  , ExecutionReceipt (..)
  , WorktreeEvidence (..)
  , ResponseState (..)
  , CancelOutcome (..)
  , requestId
  , attemptReply
  , reply
  , pollResponse
  , cancelResponse
  ) where

import Tidepool.Agent.Reply.Internal
  ( Reply
  , Replies
  , ReplyError (..)
  , RequestId
  , RequestLabel
  , RequestLabelError (..)
  , Response
  , ResponseFailure (..)
  , ResponseResult (..)
  , ExecutionReceipt (..)
  , WorktreeEvidence (..)
  , ResponseState (..)
  , CancelOutcome (..)
  , attemptReply
  , pollResponse
  , cancelResponse
  , reply
  , responseRequestId
  , requestLabel
  )

requestId :: Response result -> RequestId
requestId = responseRequestId
