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
choreGoal = "Make resume tell the truth: newest journal event wins, adopted work is re-judged by the ladder, partial micro-sequences are never adopted as complete, and a retried child updates the journal."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "resume-soundness"
    , nodeTask =
        "Fix four verified resume-soundness holes, staying inside harness-dogfooding/dev-tree/ plus tidepool-harness/tests/dogfood_harness_typecheck.rs. (1) NEWEST EVENT WINS: resumePlanFor's precedence is currently 'amendment if newest, else any outcome beats any split' — a stale outcome (seq 1) beats a newer split (seq 3) once the amendment guard fails. Rewrite the precedence to genuinely order by journal sequence number across all three kinds: whichever of outcome/split/replan is NEWEST for the branch decides (outcome → ResumeSkip, split → ResumeReplay, replan → ResumeAmend under the existing abandon/amend semantics); keep the pure signature so the Rust probe still calls it, and update amendmentIsNewest's role accordingly (it may dissolve into the new ordering — keep the export if the Rust probe names it, adapting the probe in dogfood_harness_typecheck.rs where its embedded Haskell fixtures exercise these decisions, and EXTEND those fixtures with the stale-outcome-vs-newer-split scenario so the pin test proves the fix). (2) LADDER BEFORE TRUST: recordedDone currently trusts a journaled outcome's raw constructor, but outcomes are journaled BEFORE foldLadder judges them — re-apply foldLadder (it is pure) to the journaled outcome inside recordedDone/integrationComplete so a pre-ladder Done that the ladder would fail (NoHeadMove, boundary, checks) never counts as completed for parent adoption. (3) NEVER ADOPT A PARTIAL MICRO-SEQUENCE: journal the accepted micro plan when a micro-split leaf starts running (a new journal event kind in DevTreeJournal.hs following the existing JournalEvent/JournalKind pattern — record the accepted microtask names), and in adoptOrUnfold refuse to adopt orphaned work for a plan with nodeSplit set unless the journal shows the micro sequence COMPLETED (a second event when runMicrotasks finishes with microtasksComplete, or fold the completeness into the outcome event) — an incomplete or unjournaled micro sequence re-runs through the ordinary coalgebra instead of being adopted. (4) RETRIED OUTCOMES UPDATE THE JOURNAL: retryLeaf currently merges a successful retry without re-journaling, so the journal's last word stays the original Failed; journal the retried outcome (same recordEvent path stampFold uses) so a later resume skips the child instead of resurrecting its failure. Style contract: records over positional accumulator threading, named where-helpers over inline lambda chains, follow existing haddock density. FINAL MICROTASK must be a self-consistency sweep: re-grep every symbol whose signature you changed at every call site, and verify quasiquote balance in every edited .hs file (count of '[fmt|' equals count of '|]')."
    , nodeChecks =
        [ "grep -q 'MicroSplit\\|MicroKind\\|micro' harness-dogfooding/dev-tree/DevTreeJournal.hs"
        , "grep -q 'foldLadder' harness-dogfooding/dev-tree/Harness.hs"
        , "bash -c 'test $(grep -o \"[[]fmt|\" harness-dogfooding/dev-tree/Harness.hs | wc -l) -eq $(grep -o \"|[]]\" harness-dogfooding/dev-tree/Harness.hs | wc -l)'"
        , "grep -q 'nodeSplit' harness-dogfooding/dev-tree/Harness.hs"
        ]
    , nodeBoundary =
        ["harness-dogfooding/dev-tree", "tidepool-harness/tests/dogfood_harness_typecheck.rs"]
    , nodeTolerated = []
    , nodeOnFailure = AskOperator
    , nodeSplit =
        Just
          SplitSpec
            { splitHints =
                "Split into 3-5 sequential microtasks: the journal vocabulary first (new event kind + writes at micro-plan acceptance and completion), then resumePlanFor's sequence-ordered precedence with the Rust probe fixtures extended in the same cycle, then ladder-before-trust in recordedDone plus retry re-journaling, and ALWAYS a final self-consistency sweep micro (re-grep changed signatures at all call sites; verify [fmt| / |] balance in edited files). Records over positional threading. Grep-class checks per micro; compilation is the orchestrator's post-fold gate."
            , splitMaxTasks = 5
            }
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 8, gateWiderThan = 4}

-- | The chore edit itself rides as uncommitted state in the dev tree.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = True
