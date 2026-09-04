{-# LANGUAGE FlexibleContexts #-}

-- | Typed request observation and one-shot target settlement.
module Tidepool.Agent.Reply
  ( RequestId
  , Response
  , Reply
  , Replies
  , ReplyError (..)
  , ResponseFailure (..)
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
  , ResponseState (..)
  , attemptReply
  , pollResponse
  , reply
  , responseRequestId
  )

requestId :: Response result -> RequestId
requestId = responseRequestId
