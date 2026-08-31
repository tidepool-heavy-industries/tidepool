{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot #-}

-- | Typed repository events.
--
-- The shared abstraction is an event DESCRIPTION, not a callback registry.  An
-- 'Event' is a value you can build, map, and merge before anything is
-- registered; 'withHandler' is what makes one live, for exactly the extent of
-- its lexical body.
--
-- @
-- 'withHandler' ('headChanged' parentTree) (\\change ->
--   say ("parent HEAD moved to " <> renderGitOid ('newHead' ('value' change)))
--   ) $
--   spawnAgent \@WorkerResult (spawnSpecIn (worktreeId parentTree) "parent" task)
-- @
--
-- The handler closure runs in the SAME @M effs@ environment as the code around
-- it.  It may spawn a reviewer, ask the operator, write a file, or record a
-- receipt — whatever its effect row permits.  Nothing about it is a restricted
-- callback context.
--
-- == Handler semantics
--
-- * A subscription begins at registration and never replays older events.  A
--   handler registered now will not be told about commits that already
--   happened; if it were, the dev-tree dogfood would poke every child to
--   rebase onto commits they were already built from.
-- * Events broadcast to all registered handlers.  They are not consumed by the
--   first one to see them.
-- * Each subscription invokes one handler at a time.  Later matches queue in
--   observation order, so a handler that suspends does not race its own next
--   invocation.
-- * Separate handlers interleave only at suspension points.
-- * When the body ends, intake closes, already-observed events plus any
--   in-flight handler drain, and then the subscription unregisters.
-- * Handler failure fails the enclosing scope.  It is never logged and
--   forgotten.
-- * Queue overflow, source loss, or an inability to drain fails LOUDLY.
--   Commits are never silently dropped.
--
-- == Blocking wait — 'nextEvent' and 'after'
--
-- 'nextEvent' is the ONE-SHOT, blocking sibling of 'withHandler': it
-- subscribes, blocks at the runtime until the first matching observation
-- arrives, then unsubscribes — sharing the same registry, no-replay rule,
-- queue bound, and loud-overflow semantics, but with no handler callback and
-- no caller-supplied timeout of its own. It never spins or polls at the
-- Haskell level; the block happens at the handler.
--
-- 'after' names a deadline as an 'Event': @after ms@ describes a deadline
-- @ms@ milliseconds from the moment it is SUBSCRIBED (not from this call —
-- 'after' performs no effect of its own, it is pure data construction) and
-- fires exactly one 'Tick' the first time it is found due, through the SAME
-- subscription registry as repository watches. Composing it with '<|>' is
-- how a bounded wait reads:
--
-- @
-- deadline <- 'after' 30000
-- 'nextEvent' (fmap Left ('commit' tree) '<|>' fmap Right deadline) >>= \\case
--   Observed _ (Left c)  -> ...   -- a commit landed first
--   Observed _ (Right _) -> ...   -- the deadline won
-- @
--
-- == Coalesced deltas
--
-- Observations are state deltas, not a complete movement log.  An observer
-- that finds @HEAD@ at C after last seeing A reports ONE transition even if the
-- tree passed through B, and classification degrades honestly to
-- 'UnknownChange' when the intermediate history is not recoverable.  Do not
-- write a consumer that treats this stream as exhaustive history.  The
-- dependency-propagation job needs only latest state, which coalescing
-- preserves exactly.
--
-- == Lifetime
--
-- Closures live only in the current resident cycle.  A later cycle
-- re-registers its reactions from explicit 'State' and stable 'WorktreeId's; a
-- parked Haskell closure is never the representation of pending work across a
-- cycle boundary.
--
-- == Capability mailboxes and the green-thread completion watch
--
-- 'mailbox'/'asyncDone' are event sources like 'commit'/'headChanged': possession
-- of the 'Int' is the only capability check, and both payloads stay BARE (like
-- 'Tick'), so 'nextEvent' yields a single 'Observed', never a double wrap.
--
-- A mailbox retains its keyed, coalesced sends until the FIRST matching
-- subscription consumes them, in first-arrival order (a same-key replacement
-- keeps its original position). That retained backlog is a point-to-point
-- handoff, not a replay log. Once receivers are live, later sends broadcast
-- to every live matching Event subscription just like every other Event.
-- This makes a child that sends before its parent calls 'received' observable
-- without turning a capability mailbox into a durable broker.
-- Retention has the same configured bound as an Event subscription: excess
-- distinct keys poison the first claimant with 'EventQueueOverflow', rather
-- than becoming an unbounded mailbox or disappearing silently.
--
-- == Where this module's definitions come from
--
-- `tidepool-protocol`'s closed schema represents FOUR names —
-- 'awaitSubscriptionRaw', 'mailboxNew', 'mailboxSend', 'mailboxDrop' —
-- because each is a thin wrapper over exactly one verb; those four come from
-- @Tidepool.Effects@ (re-exported below, not redefined). The rest, plus the
-- 'Event'/'Observed' TYPE declarations and the 'Functor' instance, are
-- DEFINITIONS in this module: constructor applications, `case` matches over
-- a sum's variants, `do` blocks, and recursive functions are not thin verb
-- wrappers, and 'Event'/'Observed' are genuinely polymorphic (a type
-- parameter, and 'Event' a function-typed field) with no schema vocabulary
-- to represent them.
module Tidepool.Event
  ( -- * Event descriptions
    Event
  , commit
  , projectCommit
  , headChanged
  , projectHead
  , after
  , projectTick
  , (<|>)

    -- * Observations
  , Observed (..)
  , CommitReceipt (..)
  , HeadChangeReceipt (..)
  , HeadChangeKind (..)
  , Tick (..)
  , EventId
  , SubscriptionId
  , Watch (..)
  , RepositoryEvent (..)
  , eventIdOf
  , firstMatch

    -- * Registration
  , withHandler
  , withHandlerTry
  , pumpEff
  , drainSubscription

    -- * Blocking wait
  , nextEvent
  , nextEventTry
  , awaitFirst
  , awaitSubscriptionRaw

    -- * Capability mailboxes and the green-thread completion watch
  , mailbox
  , projectMailbox
  , asyncDone
  , projectAsyncDone
  , waitEvent
  , mailboxNew
  , mailboxSend
  , mailboxDrop
  ) where

import Control.Monad.Freer hiding (run)
-- `Eff`'s own constructors, for `pumpEff`'s interposition — the SAME type
-- re-exported by `Control.Monad.Freer` above, so this import adds
-- constructors and the queue operations and shadows nothing. Exactly the
-- import the generated `Tidepool.Effects` module itself carries
-- (`tidepool-mcp/src/eval_prep.rs`), reproduced here now that `pumpEff` is
-- a definition in this module rather than spliced text in that one.
import Control.Monad.Freer.Internal (Eff (..), qApp, tsingleton)
import Tidepool.Async.Types (Async, asyncThreadId)
import Tidepool.Effects
  ( CommitReceipt (..)
  , EventError
  , EventId
  , HeadChangeKind (..)
  , HeadChangeReceipt (..)
  , M
  , RepoEvent (RepoEventDrain, RepoEventSubscribe, RepoEventUnsubscribe)
  , RepositoryEvent (..)
  , SubscriptionId
  , Tick (..)
  , Watch (..)
  , WorktreeHandle
  , WorktreeId
  , awaitSubscriptionRaw
  , liftEither
  , mailboxDrop
  , mailboxNew
  , mailboxSend
  , worktreeId
  )
-- `Tidepool.Prelude` re-exports `Control.Applicative`'s `(<|>)`
-- (`Alternative`); this module's OWN `(<|>)` (merging two `Event` sources)
-- shadows it, matching the splice-in-generated-module behavior this code had
-- before relocation — an eval importing `Tidepool.Event` sees the Event
-- merge operator, not the generic Alternative one.
import Tidepool.Prelude hiding (error, (<|>))

default (Int, Double, Text)

-- | An observation paired with the runtime identity of the reconciled fact
-- it came from — sharing one 'EventId' is how a consumer tells two views of
-- one change ('commit'/'headChanged' on the same underlying commit) from two
-- separate changes.
data Observed a = Observed { eventId :: EventId, value :: a } deriving (Show, Eq)

-- | An Event is a DESCRIPTION: what to watch, plus how to project a raw
-- observation into the author's type. Keeping the projection in the value is
-- what makes Event a lawful Functor and lets '<|>' merge two sources into ONE
-- subscription.
data Event a = Event { eventWatches :: [Watch], eventProject :: RepositoryEvent -> Maybe a }

instance Functor Event where
  fmap f e = Event e.eventWatches (\r -> fmap f (e.eventProject r))

-- | Commits observed in a managed worktree — the high-signal semantic
-- checkpoint (normal commit, merge, cherry-pick, or amend). For review,
-- test, and receipt reactions.
commit :: WorktreeHandle -> Event (Observed CommitReceipt)
commit h = Event [WatchCommit (worktreeId h)] (projectCommit (worktreeId h))

projectCommit :: WorktreeId -> RepositoryEvent -> Maybe (Observed CommitReceipt)
projectCommit w (ObservedCommit eid r) = if r.commitWorktree == w then Just (Observed eid r) else Nothing
projectCommit _ _ = Nothing

-- | Observed movement of a worktree's HEAD — advance, amend,
-- rebase/rewrite, reset, or checkout. This is the dependency-propagation
-- signal: children want a rebase poke even when their parent was itself
-- rebased. Observations are COALESCED state deltas, not a movement log.
headChanged :: WorktreeHandle -> Event (Observed HeadChangeReceipt)
headChanged h = Event [WatchHead (worktreeId h)] (projectHead (worktreeId h))

projectHead :: WorktreeId -> RepositoryEvent -> Maybe (Observed HeadChangeReceipt)
projectHead w (ObservedHeadChange eid r) = if r.headWorktree == w then Just (Observed eid r) else Nothing
projectHead _ _ = Nothing

-- | Merge two same-typed sources into ONE subscription: observations
-- from either. Subscriptions repeat for their lexical lifetime — this
-- is not one-shot. Combine with 'fmap' to keep heterogeneous selection
-- typed: `fmap Left (commit a) <|> fmap Right (headChanged b)`.
infixl 3 <|>
(<|>) :: Event a -> Event a -> Event a
l <|> r = Event (l.eventWatches ++ r.eventWatches) (\o -> case l.eventProject o of { Just a -> Just a; Nothing -> r.eventProject o })

-- The interposition. `pumpEff` recurses on the BODY only, never on
-- the tick — that asymmetry IS the one-handler-at-a-time guarantee
-- for a subscription, and it is structural rather than enforced.

-- | Run `tick` before every effect `body` performs. The scoped
-- interposition `withHandler` is built from.
pumpEff :: Eff effs () -> Eff effs a -> Eff effs a
pumpEff _ (Val a) = Val a
pumpEff tick (E u q) = tick >> E u (tsingleton (\x -> pumpEff tick (qApp q x)))

-- | Drain everything this subscription has observed since the last
-- drain, applying the handler to each match in OBSERVATION ORDER.
-- A queue overflow aborts here rather than dropping a commit.
drainSubscription :: Event a -> (a -> M ()) -> SubscriptionId -> M ()
drainSubscription ev handler sub = do
  batch <- send (RepoEventDrain sub) >>= liftEither
  mapM_ (\o -> case ev.eventProject o of { Just a -> handler a; Nothing -> pure () }) batch

-- | `withHandler event handler body` registers atomically, runs `body`
-- with the handler live, and on exit closes intake, drains what was
-- already observed, and unregisters. Registration does not block, and
-- the subscription NEVER replays events older than itself.
--
-- The handler runs in the surrounding `M` row: it may send a typed
-- message, spawn a reviewer, ask the operator, or record a receipt,
-- and it may itself suspend. Its failure fails this scope.
withHandler :: Event a -> (a -> M ()) -> M b -> M b
withHandler ev handler body = do
  sub <- send (RepoEventSubscribe ev.eventWatches) >>= liftEither
  r <- pumpEff (drainSubscription ev handler sub) body
  drainSubscription ev handler sub
  send (RepoEventUnsubscribe sub) >>= liftEither
  pure r

-- | Like 'withHandler', but returns a subscription failure and delivers later
-- drain or unsubscribe failures to the handler as 'Left' values. The body
-- result remains available as @Right@ so the caller chooses the policy.
withHandlerTry :: Event a -> (Either EventError a -> M ()) -> M b -> M (Either EventError b)
withHandlerTry ev handler body = do
  subscribed <- send (RepoEventSubscribe ev.eventWatches)
  case subscribed of
    Left err -> pure (Left err)
    Right sub -> do
      r <- pumpEff (drainSubscriptionTry ev handler sub) body
      drainSubscriptionTry ev handler sub
      unsubscribed <- send (RepoEventUnsubscribe sub)
      case unsubscribed of
        Left err -> handler (Left err)
        Right () -> pure ()
      pure (Right r)

drainSubscriptionTry :: Event a -> (Either EventError a -> M ()) -> SubscriptionId -> M ()
drainSubscriptionTry ev handler sub = do
  batch <- send (RepoEventDrain sub)
  case batch of
    Left err -> handler (Left err)
    Right observations ->
      mapM_ (\o -> case ev.eventProject o of { Just a -> handler (Right a); Nothing -> pure () }) observations

eventIdOf :: RepositoryEvent -> EventId
eventIdOf (ObservedCommit eid _) = eid
eventIdOf (ObservedHeadChange eid _) = eid
eventIdOf (ObservedTick eid _) = eid
eventIdOf (ObservedAsyncDone eid _) = eid
eventIdOf (ObservedMessage eid _ _) = eid

-- | The first batch entry `ev` projects, paired with its own EventId,
-- in observation order.
firstMatch :: Event a -> [RepositoryEvent] -> Maybe (Observed a)
firstMatch _ [] = Nothing
firstMatch ev (o:os) = case ev.eventProject o of
  Just a -> Just (Observed (eventIdOf o) a)
  Nothing -> firstMatch ev os

-- | Block until `ev` produces its first matching observation:
-- subscribe, block-await, then ALWAYS unsubscribe. The one-shot
-- sibling of `withHandler` — same registry, no-replay rule, queue
-- bound, and loud-overflow semantics — with no caller timeout: compose
-- a bound wait with `after` and `<|>`. The await itself blocks at the
-- HANDLER; the retry here only ever re-loops when a batch produced by
-- a merged, multi-source Event happens to carry no entry `ev` itself
-- projects, which is not spin-polling — each iteration is still one
-- genuine blocking round trip.
nextEvent :: Event a -> M (Observed a)
nextEvent ev = do
  sub <- send (RepoEventSubscribe ev.eventWatches) >>= liftEither
  r <- awaitFirst ev sub
  send (RepoEventUnsubscribe sub) >>= liftEither
  pure r

-- | Like 'nextEvent', but returns subscription, await, or unsubscription
-- failures as data instead of throwing them.
nextEventTry :: Event a -> M (Either EventError (Observed a))
nextEventTry ev = do
  subscribed <- send (RepoEventSubscribe ev.eventWatches)
  case subscribed of
    Left err -> pure (Left err)
    Right sub -> do
      r <- awaitFirstTry ev sub
      unsubscribed <- send (RepoEventUnsubscribe sub)
      case r of
        Left err -> pure (Left err)
        Right observed -> case unsubscribed of
          Left err -> pure (Left err)
          Right () -> pure (Right observed)

awaitFirstTry :: Event a -> SubscriptionId -> M (Either EventError (Observed a))
awaitFirstTry ev sub = do
  batch <- awaitSubscriptionRaw sub (-1)
  case batch of
    Left err -> pure (Left err)
    Right observations -> case firstMatch ev observations of
      Just observed -> pure (Right observed)
      Nothing -> awaitFirstTry ev sub

awaitFirst :: Event a -> SubscriptionId -> M (Observed a)
awaitFirst ev sub = do
  batch <- awaitSubscriptionRaw sub (-1) >>= liftEither
  case firstMatch ev batch of
    Just observed -> pure observed
    Nothing -> awaitFirst ev sub

-- | A deadline `ms` milliseconds from the moment it is SUBSCRIBED (not
-- from this call — `after` is pure data construction, no effect of its
-- own, so it needs no `Time` handler in the row), as a one-shot Event:
-- it fires exactly one Tick through the SAME subscription registry as
-- repository watches, so `nextEvent (someEvent <|> after ms)` reads as
-- an ordinary select with a timeout branch.
after :: Int -> M (Event Tick)
after ms = pure (Event [WatchDeadline ms] projectTick)

projectTick :: RepositoryEvent -> Maybe Tick
projectTick (ObservedTick _ t) = Just t
projectTick _ = Nothing

-- Capability mailboxes and the
-- green-thread completion watch. Both payloads stay BARE (like
-- `Tick`, unlike `commit`/`headChanged`), so `nextEvent` yields
-- a single `Observed`, not a double wrap.

-- | Observe messages sent into a mailbox this caller holds. Possession
-- of the Int is permission — there is no lookup-by-name or enumeration.
mailbox :: Int -> Event Value
mailbox mid = Event [WatchMailbox mid] (projectMailbox mid)

projectMailbox :: Int -> RepositoryEvent -> Maybe Value
projectMailbox mid (ObservedMessage _ m v) = if m == mid then Just v else Nothing
projectMailbox _ _ = Nothing

-- | Fires when the named green thread reaches a terminal state. Carries
-- only the thread's own id back, never its result — read the settled
-- value separately, by handle.
asyncDone :: Int -> Event Int
asyncDone tid = Event [WatchAsync tid] (projectAsyncDone tid)

projectAsyncDone :: Int -> RepositoryEvent -> Maybe Int
projectAsyncDone tid (ObservedAsyncDone _ i) = if i == tid then Just i else Nothing
projectAsyncDone _ _ = Nothing

-- | The Event-algebra sibling of @Control.Concurrent.Async@'s @waitSTM@:
-- fires once when the thread reaches a terminal state (settled OR
-- cancelled), so a select over threads, timers, and mailboxes composes as
-- one ordinary 'nextEvent' instead of needing a separate blocking primitive.
--
-- Carries the HANDLE back, never the value: the typed result stays on the
-- heap and is one immediate 'Tidepool.Async.wait' (or
-- 'Tidepool.Async.waitCatch', to observe a cancel) away — the same reason
-- @poll@\/@waitCatch@ never take the value off the thread's own settle path
-- directly.
--
-- Lives HERE, not in "Tidepool.Async", because it is 'asyncDone' composed
-- with the handle — @RepoEvent@'s substrate, which a @Green@-only row (the
-- agent session's) does not carry. Calling it therefore requires
-- @RepoEvent@ in the row alongside @Green@; "Tidepool.Async.Types" is the
-- dependency-free handle vocabulary both sides share.
waitEvent :: Async a -> Event (Async a)
waitEvent h = fmap (const h) (asyncDone (asyncThreadId h))
