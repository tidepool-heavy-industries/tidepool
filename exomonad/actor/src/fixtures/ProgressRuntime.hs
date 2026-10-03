{-# LANGUAGE DataKinds #-}
module ProgressRuntime where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Reply.Internal

newtype ProgressNote = ProgressNote Int

publishNote :: Int -> Eff '[Replies] (Either ReplyError ())
publishNote request = reportRequestProgress (ProgressSink (RequestId request)) (ProgressNote 41)

publishInt :: Int -> Eff '[Replies] (Either ReplyError ())
publishInt request = reportRequestProgress (ProgressSink (RequestId request)) (41 :: Int)

observeNote :: Int -> Eff '[Replies] Bool
observeNote request = do
  state <- pollProgress (Progress (RequestId request))
  case state of
    ProgressUpdate (ProgressCursor 1) (ProgressNote 41) -> pure True
    _ -> error "correct nominal progress observer lost its typed snapshot"

observeInt :: Int -> Eff '[Replies] Bool
observeInt request = do
  state <- pollProgress (Progress (RequestId request) :: Progress Int)
  case state of
    ProgressRejected ReplyProgressTypeMismatch -> pure True
    _ -> error "wrong nominal progress observer received an admitted snapshot"
