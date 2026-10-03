{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeApplications #-}
module ProgressBoundary where

import Control.Monad.Freer (Eff)
import Data.Coerce (coerce)
import Tidepool.Agent.Reply.Internal
import Tidepool.Agent.Watch.Internal
import Tidepool.Actor.Source

newtype ProgressNote = ProgressNote Int

data Protocol a where
  ProgressEvent :: ProgressState ProgressNote -> Protocol ()

{-# OPAQUE publish #-}
publish :: ProgressSink ProgressNote -> Eff '[Replies] (Either ReplyError ())
publish sink = reportRequestProgress sink (coerce (41 :: Int) :: ProgressNote)

{-# OPAQUE observe #-}
observe :: Progress ProgressNote -> Eff '[Replies] (ProgressState ProgressNote)
observe = pollProgress

{-# OPAQUE observeInt #-}
observeInt :: Progress Int -> Eff '[Replies] (ProgressState Int)
observeInt = pollProgress

{-# OPAQUE awaitOne #-}
awaitOne :: Progress ProgressNote -> Await (ProgressState ProgressNote)
awaitOne stream = awaitProgressAfter stream (ProgressCursor 0)

{-# OPAQUE awaitMany #-}
awaitMany :: Progress ProgressNote -> Await [ProgressState ProgressNote]
awaitMany stream = awaitAnyProgress [(stream, ProgressCursor 0)]

{-# OPAQUE source #-}
source :: Progress ProgressNote -> Source Protocol
source stream = progressSource stream ProgressEvent

{-# OPAQUE rawPublish #-}
rawPublish :: Replies (Either ReplyError ())
rawPublish = PublishProgressWith 1 (ProgressNote 41) 1

{-# OPAQUE rawObserve #-}
rawObserve :: Replies (ProgressState ProgressNote)
rawObserve = ObserveProgressWith 1 1

{-# OPAQUE rawWatch #-}
rawWatch :: Watches (ProgressState ProgressNote)
rawWatch = ObserveWatchProgressWith 1 1 1 0

{-# OPAQUE bareRaw #-}
bareRaw :: Int -> Int -> Replies (ProgressState ProgressNote)
bareRaw = ObserveProgressWith

{-# OPAQUE partialRaw #-}
partialRaw :: Int -> Replies (ProgressState ProgressNote)
partialRaw = ObserveProgressWith 1

{-# OPAQUE tickedRaw #-}
tickedRaw :: Replies (ProgressState ProgressNote)
tickedRaw = {-# SCC "raw-progress" #-} ObserveProgressWith 1 1

-- A guessed/copied numeric site must never authorize an authored Sited call.
{-# OPAQUE copiedPublisher #-}
copiedPublisher :: ProgressSink ProgressNote -> Eff '[Replies] (Either ReplyError ())
copiedPublisher sink = reportRequestProgressSited 1 sink (ProgressNote 41)

{-# OPAQUE copiedObserver #-}
copiedObserver :: Progress ProgressNote -> Eff '[Replies] (ProgressState ProgressNote)
copiedObserver = pollProgressSited 1

{-# OPAQUE copiedWatch #-}
copiedWatch :: Progress ProgressNote -> Await (ProgressState ProgressNote)
copiedWatch stream = awaitProgressAfterSited 1 stream (ProgressCursor 0)

{-# OPAQUE copiedMany #-}
copiedMany :: Progress ProgressNote -> Await [ProgressState ProgressNote]
copiedMany stream = awaitAnyProgressSited 1 [(stream, ProgressCursor 0)]

{-# OPAQUE copiedSource #-}
copiedSource :: Progress ProgressNote -> Source Protocol
copiedSource stream = progressSourceSited 1 stream ProgressEvent

{-# OPAQUE openObserver #-}
openObserver :: Progress progress -> Eff '[Replies] (ProgressState progress)
openObserver = pollProgress

{-# OPAQUE castedRaw #-}
castedRaw :: Int -> Int -> Int -> Replies (Either ReplyError ())
castedRaw = coerce
  (PublishProgressWith :: Int -> ProgressNote -> Int -> Replies (Either ReplyError ()))

{-# OPAQUE copiedIntPublisher #-}
copiedIntPublisher :: ProgressSink Int -> Eff '[Replies] (Either ReplyError ())
copiedIntPublisher sink = reportRequestProgressSited (1 {- copied-site -}) sink (41 :: Int)

{-# OPAQUE copiedRawPublisher #-}
copiedRawPublisher :: Replies (Either ReplyError ())
copiedRawPublisher = PublishProgressWith (1 {- copied-site -}) (41 :: Int) 1

{-# OPAQUE safeSibling #-}
safeSibling :: Int
safeSibling = 42

{-# OPAQUE rawAlias #-}
rawAlias :: Replies (Either ReplyError ())
rawAlias = rawPublish

{-# OPAQUE rawAliasChain #-}
rawAliasChain :: Replies (Either ReplyError ())
rawAliasChain = rawAlias
