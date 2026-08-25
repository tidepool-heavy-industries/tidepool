{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The chore: what this dev-tree run is actually asked to do, xmonad-style
-- (a sibling config module the harness proper consumes rather than
-- hardcodes). Types stay in "HarnessTypes"; this module owns only the
-- VALUES — the goal, the plan tree, the budget, and the dirty-source flag —
-- so swapping a chore is an edit here, never a change to the harness's own
-- logic.
--
-- The shipped chore is THE RESTRUCTURE: dev-tree becomes a real
-- multi-module Haskell project (operator directive: serious,
-- well-structured, fluent Haskell — module boundaries where the section
-- headers already are), executed by dev-tree on its own source.
module Chore
  ( choreGoal
  , chorePlan
  , choreBudget
  , choreSnapshotDirtySource
  ) where

import HarnessTypes (Budget (..), DevPlan (..), OnFailure (..), SplitSpec (..))
import Tidepool.Prelude

choreGoal :: Text
choreGoal = "Restructure dev-tree from one ~2100-line Harness.hs into a well-factored multi-module Haskell project with fluent idioms, keeping behavior and the public API identical."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "restructure"
    , nodeTask =
        ("Split harness-dogfooding/dev-tree/Harness.hs into sibling modules along its existing section headers, keeping ALL behavior semantically identical and Harness.hs's module export list byte-compatible (Harness becomes the facade: it keeps loop/resumeLoop/initialState plus re-exports everything it exports today, because tidepool-harness/tests/dogfood_harness_typecheck.rs's embedded probes import Harness and name those exports — that file is in-boundary ONLY for a change a re-export genuinely cannot satisfy). Target modules (indicative; adjust if the dependency graph argues otherwise, and say so in evidence): Prompts.hs (the fragment grammar + per-role prompt templates), Micro.hs (microLeaf/runMicrotasks/MicroAcc + recon machinery), Resume.hs (the resume section: ResumePlan/SplitRecord/resumePlanFor/newestEntry/adoptOrUnfold/verifyOrphan typestates/retainWorktree), Fold.hs (integrate/stampFold/foldLadder/leafFold/interiorFold/foldChildren/cascade/escalate/applyPolicy/retry/mergeChild/finishFold), Unfold.hs (decompose/emitSplit/policy slots/allocateChildren), and a small Workers.hs for the shared execution seam (runWorker/snapshotWork/gitIn/runCheckCmd/runChecks/boundaryViolations) that both Fold and Micro import — shared helpers get ONE home, never copies. Runtime-only types (NodeSeed/NodeWork/FoldAcc/MicroAcc) move with their consumers. Every module gets a haddock header in the file's existing style stating its charter. Fluent idioms where they genuinely improve the code: when/unless over if-then-else-pure-unit, foldM/traverse where a manual loop is a disguised fold, catMaybes/mapMaybe chains, records over positional threading — but NO semantic changes riding along. Also ADD one new prompt fragment to Prompts.hs, haskellIdiomContract :: Text, included by the worker and microtask briefs for Haskell-editing work: it states the positive idiom expectations (records over positional argument threading; when/unless; name your where-helpers; operator expressions and if-then-else are fine inside fmt" <> "| holes; custom typeclasses and GADTs compile here — write the idiomatic version first) — mirroring how the git contract fragment is stated. Every microtask must leave the tree in an importable state (no module referencing a symbol that has not moved yet).")
    , nodeChecks =
        [ "test -s harness-dogfooding/dev-tree/Prompts.hs"
        , "test -s harness-dogfooding/dev-tree/Micro.hs"
        , "test -s harness-dogfooding/dev-tree/Resume.hs"
        , "test -s harness-dogfooding/dev-tree/Fold.hs"
        , "test -s harness-dogfooding/dev-tree/Unfold.hs"
        , "grep -q 'haskellIdiomContract' harness-dogfooding/dev-tree/Prompts.hs"
        , "grep -q 'resumePlanFor' harness-dogfooding/dev-tree/Harness.hs"
        , "bash -c 'for f in harness-dogfooding/dev-tree/*.hs; do a=$(grep -o \"\\[fmt" <> "|" <> "\" $f | wc -l); b=$(grep -o \"|\\]\" $f | wc -l); test $a -eq $b || exit 1; done'"
        , "bash -c 'test $(wc -l < harness-dogfooding/dev-tree/Harness.hs) -lt 700'"
        ]
    , nodeBoundary =
        ["harness-dogfooding/dev-tree", "tidepool-harness/tests/dogfood_harness_typecheck.rs"]
    , nodeTolerated = []
    , nodeOnFailure = AskOperator
    , nodeSplit =
        Just
          SplitSpec
            { splitHints =
                "Sequential carve-outs, one module per microtask, dependency-leaves first so the tree is importable after every cycle: (1) Prompts.hs + the new haskellIdiomContract fragment wired into the briefs; (2) Workers.hs (shared execution seam) with Harness re-exporting; (3) Micro.hs; (4) Resume.hs; (5) Fold.hs + Unfold.hs together (they share the integrate/decompose seam) and Harness.hs reduced to facade + loop/resumeLoop/initialState + re-exports. The FINAL act of microtask 5 doubles as the self-consistency sweep: re-grep every moved symbol's import/export at every use site and verify quasiquote balance in every file. Do not attempt Haskell compilation — the orchestrator gates it after the fold."
            , splitMaxTasks = 5
            }
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 10, gateWiderThan = 4}

-- | The chore edit itself rides as uncommitted state in the dev tree.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = True
