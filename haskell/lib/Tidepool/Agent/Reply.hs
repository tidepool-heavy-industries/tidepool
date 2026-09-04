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
  , requestId
  , attemptReply
  , reply
  , pollResponse
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
  , attemptReply
  , pollResponse
  , reply
  , responseRequestId
  , requestLabel
  )

requestId :: Response result -> RequestId
requestId = responseRequestId
