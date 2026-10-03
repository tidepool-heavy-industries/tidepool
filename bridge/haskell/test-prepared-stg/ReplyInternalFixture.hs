{-# LANGUAGE GADTs #-}

-- Keep the authority lookup focused on the protected constructor itself.
module Tidepool.Agent.Reply.Internal (ReplyError(..), Replies(..)) where

data ReplyError = ReplyError

data Replies progress where
  PublishProgressWith :: Int -> progress -> Replies (Either ReplyError ())
