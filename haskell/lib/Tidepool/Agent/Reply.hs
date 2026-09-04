{-# LANGUAGE FlexibleContexts #-}

-- | Typed request observation and one-shot target settlement.
module Tidepool.Agent.Reply
  ( RequestId
  , Response
  , Reply
  , Replies
  , ReplyError (..)
  , ResponseFailure (..)
  , ResponseResult (..)
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
  , Response
  , ResponseFailure (..)
  , ResponseResult (..)
  , WorktreeEvidence (..)
  , ResponseState (..)
  , attemptReply
  , pollResponse
  , reply
  , responseRequestId
  )

requestId :: Response result -> RequestId
requestId = responseRequestId
