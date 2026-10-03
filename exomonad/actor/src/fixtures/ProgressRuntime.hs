{-# LANGUAGE DataKinds #-}
module ProgressRuntime where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Reply.Internal
import Tidepool.Actor.Source (installSource, progressSource)
import Tidepool.Effects.Core (ActorKernel)

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

data SourceCheck result = SourceCheck Bool

installIntSource :: Int -> Eff '[ActorKernel, Replies] ()
installIntSource request = installSource
  (progressSource (Progress (RequestId request) :: Progress Int) project)
  where
    project (ProgressRejected ReplyProgressTypeMismatch) = SourceCheck True
    project _ = SourceCheck False

installNoteSource :: Int -> Eff '[ActorKernel, Replies] ()
installNoteSource request = installSource
  (progressSource (Progress (RequestId request) :: Progress ProgressNote) project)
  where
    project (ProgressUpdate (ProgressCursor revision) (ProgressNote 41)) = SourceCheck (revision > 0)
    project ProgressClosed = SourceCheck True
    project _ = SourceCheck False

verifySource :: SourceCheck () -> Eff '[Replies] ()
verifySource (SourceCheck True) = pure ()
verifySource _ = error "source mapper received the wrong nominal progress state"
