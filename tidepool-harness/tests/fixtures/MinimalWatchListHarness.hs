{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

-- | THE SMALLEST KNOWN REPRODUCER of the tenure-then-resume rooting gap
-- (PRD 20 S1-L4; see `tidepool-runtime/tests/nested_async_repro.rs`'s
-- module doc for the full mechanism writeup). Bisected down from
-- `Tidepool.Node.forkNode` (`node_mailboxes.rs`'s fixture) by removing,
-- one piece at a time, everything not load-bearing for the crash:
--
-- * No `sendUp`, no `received`, no deadline select, no `Tidepool.Node` at
--   all — just two `mailboxNew`-derived Ints and one `async`.
-- * No `wait`/`waitCatch` protocol required — the SPAWNER's OWN resume
--   traps directly (confirmed by an even smaller variant with `wait`
--   dropped entirely — not committed, since this file already reproduces
--   without it needing removal here).
-- * No `Event`/`fmap` composition required, and no CLOSURE capture at
--   all — `asyncBody` captures ONLY a bare list, `[WatchMailbox downMid]`
--   (one cons cell wrapping a 1-field Con), inside the closure AsyncSpawnWith
--   tenures. That list is never read inside the child body.
--
-- So the family's own name ("a NodeCtx whose inbox field is itself a
-- closure") undersells the trigger: a CLOSURE isn't needed, only a LIST
-- reachable from the tenured closure's transitive graph, alongside sibling
-- Ints captured via ordinary handled-effect responses.
--
-- # What this does NOT resolve
--
-- Hand-built `CoreExpr` repros in `tidepool-runtime/tests/tenure_resume_gc_repro.rs`
-- reconstruct this EXACT shape — the same list Con, the same sibling
-- Int captures via a handled dispatch, the same suspend/tenure/resume
-- order, even real in-flight GC via a byte-scale nursery — and all of them
-- PASS cleanly under `TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY`. The
-- rooting discipline (stowed roots, persistent roots, `RootedLocal`/
-- `RootedStack`, the write barrier) is verified SOUND for every structural
-- variant reachable through hand-built `CoreExpr`. Whatever the real GHC
-- pipeline produces here differs from every hand-built reconstruction in a
-- way not yet identified — plausibly a JIT-codegen (Cranelift stack-map
-- coverage) difference rather than a Rust-side rooting gap. See this lane's
-- `notify_parent` report for the full diagnosis.
--
-- GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH.
module MinimalWatchListHarness
  ( State (..)
  , initialState
  , render
  , loop
  ) where

import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Async (async, wait)
import Tidepool.Effects (Watch (..), liftEither, mailboxNew)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

import Tidepool.Harness (Harness)

data State = State
  { runs :: Int
  }
  deriving (Generic, Show, Eq, FromJSON, ToJSON)

initialState :: State
initialState = State {runs = 0}

render :: State -> Text
render st = [fmt|Minimal watch-list probe. Runs: {runs st}.|]

-- | Captures `[WatchMailbox downMid]` (a bare list, no closure) but never
-- reads it — matching `forkNode`'s own `NodeCtx` construction, which the
-- body it hands to is equally free to ignore.
asyncBody :: Int -> [Watch] -> Harness Int
asyncBody _upMid _watches = pure 0

loop :: State -> Harness State
loop st = do
  downMid <- mailboxNew >>= liftEither
  upMid <- mailboxNew >>= liftEither
  thread <- async (asyncBody upMid [WatchMailbox downMid])
  _h <- wait thread
  pure st {runs = st.runs + 1}
