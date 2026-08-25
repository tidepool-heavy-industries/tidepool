{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The chore: what this dev-tree run is actually asked to do, xmonad-style
-- (a sibling config module the harness proper consumes rather than
-- hardcodes). Types stay in "HarnessTypes"; this module owns only the
-- VALUES — the goal, the plan tree, the budget, and the dirty-source flag —
-- so swapping a chore is an edit here, never a change to the harness's own
-- logic.
--
-- The shipped chore is TIDEPOOL-ON-TIDEPOOL: dev-tree fixing a real bug in
-- its own stdlib — the bug dev-tree's first live run discovered
-- (Tidepool.Event's helpers liftEither every EventError, so any event
-- failure is process-fatal instead of data the failure policy reads).
-- Point TIDEPOOL_SOURCE_REPO at the tidepool dev checkout to run it.
module Chore
  ( choreGoal
  , chorePlan
  , choreBudget
  , choreSnapshotDirtySource
  ) where

import HarnessTypes (Budget (..), DevPlan (..), OnFailure (..))
import Tidepool.Prelude

choreGoal :: Text
choreGoal = "Give Tidepool.Event a non-fatal error surface: Try variants that return EventError as data instead of throwing out of the harness loop."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "event-try-surface"
    , nodeTask =
        "In haskell/lib/Tidepool/Event.hs ONLY, add two non-fatal variants alongside the existing throwing helpers (which must stay byte-for-byte unchanged): `withHandlerTry :: Event a -> (Either EventError a -> M ()) -> M b -> M (Either EventError b)` and `nextEventTry :: Event a -> M (Either EventError (Observed a))`. Semantics: a subscribe failure returns Left immediately and the body/wait never runs; a drain failure mid-body and an unsubscribe failure at exit are delivered to the HANDLER as Left values (the body's result stays total, so the caller's policy decides — nothing is ever thrown). For nextEventTry, any failure is the returned Left. Follow the file's existing style exactly (haddock density, naming, the runInTry precedent from Tidepool.Shell for the -Try suffix). Export both from the module head next to their throwing siblings. Do not modify any other file, and do not change any existing function."
    , nodeChecks =
        [ "grep -q 'withHandlerTry ::' haskell/lib/Tidepool/Event.hs"
        , "grep -q 'nextEventTry ::' haskell/lib/Tidepool/Event.hs"
        , "grep -q 'withHandlerTry' haskell/lib/Tidepool/Event.hs && grep -A40 'module Tidepool.Event' haskell/lib/Tidepool/Event.hs | grep -q 'withHandlerTry'"
        ]
    , nodeBoundary = ["haskell/lib/Tidepool/Event.hs"]
    , nodeOnFailure = AskOperator
    , nodeSplit = Nothing
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 2, gateWiderThan = 4}

-- | The dev tree carries in-flight (uncommitted) morning surface work; the
-- run snapshots it rather than requiring a clean source.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = True
