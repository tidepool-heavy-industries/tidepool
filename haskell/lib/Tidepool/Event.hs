{-# LANGUAGE OverloadedStrings #-}

-- | Typed repository events — the authored surface of
-- @plans\/self-iterating-harness\/19-managed-worktrees-events-prd.md@.
--
-- The shared abstraction is an event DESCRIPTION, not a callback registry.  An
-- 'Event' is a value you can build, map, and merge before anything is
-- registered; 'withHandler' is what makes one live, for exactly the extent of
-- its lexical body.
--
-- @
-- 'withHandler' ('headChanged' parentTree) (\\change ->
--   for_ childAgents $ \\child ->
--     pokeAgent child (whenSafe (Rebase (newHead (value change))))
--   ) $ do
--     parent <- spawnAgent parentSpec parentTask
--     waitAgent parent
-- @
--
-- The handler closure runs in the SAME @M effs@ environment as the code around
-- it.  It may send a typed message, spawn a reviewer, ask the operator, or
-- record a receipt — whatever its effect row permits.  Nothing about it is a
-- restricted callback context.
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
module Tidepool.Event
  ( -- * Event descriptions
    Event
  , commit
  , headChanged
  , (<|>)

    -- * Observations
  , Observed (..)
  , CommitReceipt (..)
  , HeadChangeReceipt (..)
  , HeadChangeKind (..)
  , EventId

    -- * Registration
  , withHandler
  ) where

import Tidepool.Effects
  ( CommitReceipt (..)
  , Event
  , EventId
  , HeadChangeKind (..)
  , HeadChangeReceipt (..)
  , Observed (..)
  , commit
  , headChanged
  , withHandler
  , (<|>)
  )
