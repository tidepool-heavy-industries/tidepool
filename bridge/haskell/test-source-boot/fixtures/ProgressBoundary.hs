{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeApplications #-}
module ProgressBoundary where

import Control.Monad.Freer (Eff)
import Data.Coerce (coerce)
import Tidepool.Agent.Reply.Internal
import Tidepool.Agent.Watch.Internal
import Tidepool.Actor.Source
import Tidepool.Internal.RequestSite (RequestSite)

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
rawPublish = PublishProgressWith (error "unavailable carrier") (ProgressNote 41) 1

{-# OPAQUE rawObserve #-}
rawObserve :: Replies (ProgressState ProgressNote)
rawObserve = ObserveProgressWith (error "unavailable carrier") 1

{-# OPAQUE rawWatch #-}
rawWatch :: Watches (ProgressState ProgressNote)
rawWatch = ObserveWatchProgressWith (error "unavailable carrier") 1 1 0

{-# OPAQUE bareRaw #-}
bareRaw :: RequestSite '[ProgressNote] (ProgressState ProgressNote) -> Int -> Replies (ProgressState ProgressNote)
bareRaw = ObserveProgressWith

{-# OPAQUE partialRaw #-}
partialRaw :: Int -> Replies (ProgressState ProgressNote)
partialRaw = ObserveProgressWith 1

{-# OPAQUE tickedRaw #-}
tickedRaw :: Replies (ProgressState ProgressNote)
tickedRaw = {-# SCC "raw-progress" #-} ObserveProgressWith (error "unavailable carrier") 1

-- A raw/Sited reference cannot escape compiler-issued helper admission.
{-# OPAQUE copiedPublisher #-}
copiedPublisher :: ProgressSink ProgressNote -> Eff '[Replies] (Either ReplyError ())
copiedPublisher sink = reportRequestProgressSited (error "unavailable carrier") sink (ProgressNote 41)

{-# OPAQUE copiedObserver #-}
copiedObserver :: Progress ProgressNote -> Eff '[Replies] (ProgressState ProgressNote)
copiedObserver = pollProgressSited (error "unavailable carrier")

{-# OPAQUE copiedWatch #-}
copiedWatch :: Progress ProgressNote -> Await (ProgressState ProgressNote)
copiedWatch stream = awaitProgressAfterSited (error "unavailable carrier") stream (ProgressCursor 0)

{-# OPAQUE copiedMany #-}
copiedMany :: Progress ProgressNote -> Await [ProgressState ProgressNote]
copiedMany stream = awaitAnyProgressSited (error "unavailable carrier") [(stream, ProgressCursor 0)]

{-# OPAQUE copiedSource #-}
copiedSource :: Progress ProgressNote -> Source Protocol
copiedSource stream = progressSourceSited (error "unavailable carrier") stream ProgressEvent

{-# OPAQUE openObserver #-}
openObserver :: Progress progress -> Eff '[Replies] (ProgressState progress)
openObserver = pollProgress

{-# OPAQUE castedRaw #-}
castedRaw :: RequestSite '[ProgressNote] (Either ReplyError ()) -> Int -> Int -> Replies (Either ReplyError ())
castedRaw = coerce
  (PublishProgressWith :: RequestSite '[ProgressNote] (Either ReplyError ()) -> ProgressNote -> Int -> Replies (Either ReplyError ()))

{-# OPAQUE copiedIntPublisher #-}
copiedIntPublisher :: ProgressSink Int -> Eff '[Replies] (Either ReplyError ())
copiedIntPublisher sink = reportRequestProgressSited (error "unavailable carrier") sink (41 :: Int)

{-# OPAQUE copiedRawPublisher #-}
copiedRawPublisher :: Replies (Either ReplyError ())
copiedRawPublisher = PublishProgressWith (error "unavailable carrier") (41 :: Int) 1

{-# OPAQUE safeSibling #-}
safeSibling :: Int
safeSibling = 42

{-# OPAQUE rawAlias #-}
rawAlias :: Replies (Either ReplyError ())
rawAlias = rawPublish

{-# OPAQUE rawAliasChain #-}
rawAliasChain :: Replies (Either ReplyError ())
rawAliasChain = rawAlias
