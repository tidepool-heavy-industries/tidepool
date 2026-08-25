{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The chore: what this dev-tree run is actually asked to do, xmonad-style
-- (a sibling config module the harness proper consumes rather than
-- hardcodes). Types stay in "HarnessTypes"; this module owns only the
-- VALUES — the goal, the plan tree, the budget, and the dirty-source flag —
-- so swapping a chore is an edit here, never a change to the harness's own
-- logic.
--
-- The shipped chore is dev-tree EDITING ITS OWN SOURCE: a micro-split leaf
-- against the dev checkout that teaches the harness's boundary vocabulary a
-- second tier (product paths vs tolerated paths) — the #1 false-red source
-- across the live runs, chosen from the observation-driven leverage list.
module Chore
  ( choreGoal
  , chorePlan
  , choreBudget
  , choreSnapshotDirtySource
  ) where

import HarnessTypes (Budget (..), DevPlan (..), OnFailure (..), SplitSpec (..))
import Tidepool.Prelude

choreGoal :: Text
choreGoal = "Refactor dev-tree's prompt layer into compositional fmt fragments: every trust-contract obligation stated exactly once, prompts as templates whose holes name fragments."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "prompt-fragments"
    , nodeTask =
        "Refactor the prompt section of harness-dogfooding/dev-tree/Harness.hs (the \"Prompts — the one place typed orchestration becomes prose\" section) so every shared trust-contract obligation is a NAMED fragment stated exactly once, and each per-role prompt is one [fmt|...|] template whose holes name those fragments. This is a Writing Good Code exercise: the section should read as a small document grammar — fmt blocks naming other fmt blocks, compositional, no <> assembly chains for prompt bodies. Concretely: (1) introduce haddocked fragment values/functions above the prompts, one per shared obligation — the sandbox-git contract (git dir read-only, do NOT run git commit/add, orchestrator snapshots), the orchestrator-runs-checks contract including the your-shell-has-no-ghc sentence (parameterized over the rendered check lines, since node checks and microtask checks render from different sources), the boundary contract (product paths + tolerated tier, parameterized over the plan), and the WorkerResult finishing contract including evidence-never-a-sha, obstacles, and frictionNotes (parameterized over the role-specific what-to-describe phrase). (2) Rewrite workerPrompt, scaffoldPrompt, integrationPrompt, and microPrompt as single [fmt|...|] templates that interpolate those fragments by name; reconPrompt, microPlanPrompt, resolutionPrompt, and replanPrompt keep their own text but adopt any fragment that genuinely applies. (3) The rendered prompts must preserve every obligation and all task-specific content; wording may unify where duplicates drifted. (4) Success is mechanically visible: each of these phrases appears EXACTLY ONCE in the whole file afterwards — 'do NOT run', 'never a commit sha', 'Include obstacles', 'shell has no ghc' (baseline today: 4, 3, 4, 2). Do not change any type, any non-prompt function, or any other file."
    , nodeChecks =
        [ "test 1 -eq $(grep -c 'do NOT run' harness-dogfooding/dev-tree/Harness.hs)"
        , "test 1 -eq $(grep -c 'never a commit sha' harness-dogfooding/dev-tree/Harness.hs)"
        , "test 1 -eq $(grep -c 'Include obstacles' harness-dogfooding/dev-tree/Harness.hs)"
        , "test 1 -eq $(grep -c 'shell has no ghc' harness-dogfooding/dev-tree/Harness.hs)"
        ]
    , nodeBoundary = ["harness-dogfooding/dev-tree/Harness.hs"]
    , nodeTolerated = []
    , nodeOnFailure = AskOperator
    , nodeSplit =
        Just
          SplitSpec
            { splitHints =
                "Split into 2-3 sequential microtasks: carve out the named fragments first (with the four heavy prompts rewritten to use them, since fragments without consumers are dead text), then the remaining prompts' adoption pass, then a final duplication-count sweep against the exact-once phrases. Cheap grep-count checks per microtask; GHC compilation is the orchestrator's gate, your shell has none."
            , splitMaxTasks = 3
            }
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 6, gateWiderThan = 4}

-- | The chore edit itself rides as uncommitted state in the dev tree.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = True
