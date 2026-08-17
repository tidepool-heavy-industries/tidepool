//! The authored dogfood harnesses in `harness-dogfooding/` typecheck against
//! today's generated effect surface.
//!
//! THE HAZARD. These are Haskell sources the driver loads at RUNTIME, not
//! cargo targets — nothing else in the workspace compiles them, so `cargo
//! check`, clippy, and the whole Rust suite are all blind to them. An authored
//! harness can therefore drift arbitrarily far from the effect surface it
//! calls (a verb that no longer exists, a record field renamed, a LOCKED
//! signature quietly changed, a sum arm the generic-JSON derive rejects) and
//! stay green indefinitely. `examples/harness/` is covered by ~8 driver tests;
//! without this file the `harness-dogfooding/` harnesses are covered by none.
//!
//! COST. Two extract compiles, one per harness, because the two need
//! DIFFERENT effect rows and a row is what a compile is parameterized by —
//! there is no bundling that avoids the second. Both live in the GHC-heavy
//! tier (`.config/nextest.toml`'s default-filter excludes
//! `package(tidepool-harness) & kind(test)` wholesale).
//!
//! SCOPE. Typecheck only. These probes force GHC through `render` and `loop`
//! — plus `resumeLoop` and the pure resume decisions for a harness that
//! declares them; they do not drive a model, spawn an agent, or touch a
//! repository.

mod support;

use std::path::PathBuf;
use tidepool_harness::engine::compile_turn;
use tidepool_harness::engine::EngineConfig;
use tidepool_runtime::compile_and_run_pure;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

/// Compile a probe module that imports the harness at `harness_dir` against
/// `decls`, forcing GHC through both locked entry points (`render` and
/// `loop`). Importing the module is already enough to typecheck every
/// declaration in it — GHC compiles a module whole — but naming both keeps
/// the probe honest about which contract is under test.
///
/// `extra_imports` and `extra_decls` are spliced into the probe's import block
/// and body respectively, for a harness that declares MORE than the universal
/// contract (dev-tree's `resumeLoop` and its pure resume decisions).
fn typecheck(
    harness_dir: &str,
    decls: Vec<tidepool_mcp::EffectDecl>,
    extra_imports: &str,
    extra_decls: &str,
) {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    let cfg = EngineConfig::from_decls(
        decls,
        repo_root().join("haskell/lib"),
        Some(repo_root().join(harness_dir)),
    )
    .expect("engine config for the dogfood row");
    let source = format!(
        concat!(
            "{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, ",
            "FlexibleContexts, GADTs, ScopedTypeVariables, TypeApplications, LambdaCase, ",
            "RecordWildCards, OverloadedRecordDot, QuasiQuotes, DeriveGeneric, DeriveAnyClass #-}}\n",
            "module Probe where\n",
            "import Tidepool.Prelude hiding (render)\n",
            "import Tidepool.Effects\n",
            "import Harness\n",
            "{extra_imports}",
            "__probe :: M Text\n",
            "__probe = do {{ st <- loop initialState; pure (render st) }}\n",
            "{extra_decls}",
        ),
        extra_imports = extra_imports,
        extra_decls = extra_decls,
    );
    if let Err(e) = compile_turn(&cfg.extract_bin, &source, "__probe", &cfg.include, 0, 0) {
        panic!("{harness_dir} does not typecheck against today's surface:\n{e}");
    }
}

/// The companion's row is the driver's own outer session: `[RunLLMTurn,
/// AskUser, Worktree, Subagent]` (mirrors `selfharness::driver::outer_decls`
/// — interposed effects FIRST so the suspend threshold stays 0; Worktree is
/// Subagent's hard companion). Its `loop` calls only `runLLMTurn` today —
/// conversation happens through the answerer's own `askUser` forms — but the
/// probe compiles against the full outer row the driver actually serves.
#[test]
fn companion_typechecks() {
    typecheck(
        "harness-dogfooding/companion",
        vec![
            tidepool_mcp::runllmturn_decl(),
            tidepool_mcp::askuser_decl(),
            tidepool_mcp::worktree_decl(),
            tidepool_mcp::subagent_decl(),
        ],
        "",
        "",
    );
}

/// dev-tree's full outer row (mirrors `selfharness::driver::outer_decls`),
/// shared by the typecheck probe below and the execution test further down
/// so the two never compile the harness against different rows.
fn dev_tree_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::runllmturn_decl(),
        tidepool_mcp::askuser_decl(),
        tidepool_mcp::console_decl(),
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::event_decl(),
        tidepool_mcp::exec_decl(),
        tidepool_mcp::subagent_decl(),
        tidepool_mcp::journal_decl(),
    ]
}

/// dev-tree is the executable design target of S1-L1: its row now IS the
/// driver's widened outer session (`[RunLLMTurn, AskUser, Console, Worktree,
/// RepoEvent, Exec, Subagent, Journal]` — mirrors
/// `selfharness::driver::outer_decls`, interposed effects FIRST so the
/// suspend threshold stays 0). Typechecking against that exact row is what
/// turns this from "the file names only landed API" into "this compiles
/// against what the driver actually serves".
///
/// dev-tree also declares the OPT-IN second entry point (PRD 20 S1-L5's
/// `resumeLoop :: ResumeFold -> State -> Harness State`), so the probe names
/// both entries: a fresh boot compiles `loop`, a boot with a folded journal
/// compiles `resumeLoop`, and the driver picks between them. Naming
/// `resumeLoop` at its exact declared signature is what makes a drift in
/// either half a compile failure here rather than a boot refusal in
/// production.
///
/// The two pure resume decisions are named at their signatures for the same
/// reason the coalgebra's pure policy slots are pure: they decide from the
/// fold alone, with no agent and no git anywhere in the path, so they are
/// callable directly.
#[test]
fn dev_tree_typechecks() {
    typecheck(
        "harness-dogfooding/dev-tree",
        dev_tree_decls(),
        "import Tidepool.Resume (ResumeFold, emptyResume)\n",
        concat!(
            "__resumeProbe :: M Text\n",
            "__resumeProbe = do { st <- resumeLoop emptyResume initialState; pure (render st) }\n",
            "__resumeDecision :: ResumeFold -> Text -> DevPlan -> ResumePlan\n",
            "__resumeDecision = resumePlanFor\n",
            "__amendmentNewest :: Maybe Int -> Maybe Int -> Maybe Int -> Bool\n",
            "__amendmentNewest = amendmentIsNewest\n",
        ),
    );
}

/// Compile a PURE (no-`Eff`) target against `harness_dir` and run it on the
/// real JIT, returning its result as JSON (`tidepool_runtime::render::EvalResult::to_json`).
///
/// `decls` still has to name the harness's full assumed effect row — GHC
/// compiles the imported module WHOLE, so even a target that only touches
/// pure decision functions needs `Tidepool.Effects` generated at the row the
/// harness file itself was authored against, or the module fails to
/// typecheck before the pure part is ever reached.
fn execute_pure(
    harness_dir: &str,
    decls: Vec<tidepool_mcp::EffectDecl>,
    source: &str,
    target: &str,
) -> serde_json::Value {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    let cfg = EngineConfig::from_decls(
        decls,
        repo_root().join("haskell/lib"),
        Some(repo_root().join(harness_dir)),
    )
    .expect("engine config for the dogfood row");
    let include: Vec<_> = cfg.include.iter().map(|p| p.as_path()).collect();
    match compile_and_run_pure(source, target, &include) {
        Ok(result) => result.to_json(),
        Err(e) => panic!("{harness_dir}'s resume decisions did not run cleanly:\n{e}"),
    }
}

/// One `module ResumeDecisionProbe where` source: hand-built `ResumeFold`
/// values covering the decision table `resumePlanFor`/`amendmentIsNewest`
/// actually implement (read from `Harness.hs` directly, not assumed), folded
/// into one newline-separated `PASS`/`FAIL` report per case.
const RESUME_DECISION_SOURCE: &str = concat!(
    "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, ",
    "FlexibleContexts, GADTs, ScopedTypeVariables, TypeApplications, LambdaCase, ",
    "RecordWildCards, OverloadedRecordDot, QuasiQuotes, DeriveGeneric, DeriveAnyClass #-}\n",
    "module ResumeDecisionProbe where\n",
    "import Tidepool.Prelude hiding (render)\n",
    "import Tidepool.Effects\n",
    "import Harness\n",
    "import HarnessTypes (OnFailure (..), FoldReceipt (..), Failure (..), FailureKind (..), ReplanDecision (..))\n",
    "import Tidepool.Resume (ResumeFold (..), ResumeEntry (..), emptyResume, isResumed)\n",
    "import Tidepool.Aeson (Value, object, toJSON, (.=))\n",
    "import qualified Data.Text as T\n",
    "\n",
    "leafPlan :: DevPlan\n",
    "leafPlan = DevPlan { nodeName = \"leaf\", nodeTask = \"implement the leaf\", nodeChecks = [], nodeBoundary = [], nodeOnFailure = Retry, childPlans = [] }\n",
    "\n",
    "leafBranch :: Text\n",
    "leafBranch = \"dev-tree/leaf\"\n",
    "\n",
    "mkReceipt :: Text -> Text -> FoldReceipt\n",
    "mkReceipt node branch = FoldReceipt { receiptNode = node, receiptBranch = branch, receiptSeedHead = \"seed0000\", receiptHead = \"head1111\", receiptHeadMoved = True, receiptChecks = [], receiptRebases = [], receiptOutside = [], receiptCycles = 1, receiptAgentRan = True, receiptReviewed = False, receiptSummary = \"mock done\", receiptEvidence = [] }\n",
    "\n",
    "mkEntry :: Int -> Text -> Text -> Value -> ResumeEntry\n",
    "mkEntry sq kind key payload = ResumeEntry { resumeSeq = sq, resumeKind = kind, resumeKey = key, resumePayload = payload }\n",
    "\n",
    "mkFold :: [ResumeEntry] -> ResumeFold\n",
    "mkFold es = ResumeFold { resumeRunId = \"test-run\", resumeEntries = es }\n",
    "\n",
    "splitPayloadFor :: Text -> DevPlan -> Text -> Value\n",
    "splitPayloadFor node p scaffoldHead = object [ \"node\" .= node, \"scaffoldHead\" .= scaffoldHead, \"children\" .= map nodeName (childPlans p), \"plan\" .= toJSON p ]\n",
    "\n",
    "verify :: Text -> Bool -> Text\n",
    "verify name ok = (if ok then \"PASS: \" else \"FAIL: \") <> name\n",
    "\n",
    "-- A recorded outcome for a branch: that subtree is skipped, not re-entered.\n",
    "caseRecordedOutcome :: Text\n",
    "caseRecordedOutcome =\n",
    "  let receipt = mkReceipt \"leaf\" leafBranch\n",
    "      fold = mkFold [mkEntry 1 \"outcome\" leafBranch (toJSON receipt)]\n",
    "      expected = ResumeSkip (Done \"leaf\" [] receipt)\n",
    "  in verify \"recorded-outcome-is-skipped\" (resumePlanFor fold leafBranch leafPlan == expected)\n",
    "\n",
    "-- A recorded split with no outcome REPLAYS the recorded plan rather than\n",
    "-- being re-derived.\n",
    "caseRecordedSplitReplays :: Text\n",
    "caseRecordedSplitReplays =\n",
    "  let splitPlan = leafPlan { childPlans = [leafPlan { nodeName = \"child\" }] }\n",
    "      payload = splitPayloadFor \"leaf\" splitPlan \"scaffold0\"\n",
    "      fold = mkFold [mkEntry 1 \"split\" leafBranch payload]\n",
    "      expected = ResumeReplay SplitRecord { splitNode = \"leaf\", splitScaffoldHead = \"scaffold0\", splitPlan = splitPlan, splitChildTrees = [] }\n",
    "  in verify \"recorded-split-replays-not-rederived\" (resumePlanFor fold leafBranch leafPlan == expected)\n",
    "\n",
    "-- A replan NEWER than the split it amends drives the re-unfold.\n",
    "caseReplanNewerAmends :: Text\n",
    "caseReplanNewerAmends =\n",
    "  let splitPlan = leafPlan { nodeTask = \"original task\" }\n",
    "      splitPayload = splitPayloadFor \"leaf\" splitPlan \"scaffold1\"\n",
    "      decision = ReplanDecision { amendedInstruction = \"do it the other way\", abandonSubtree = False, rationale = \"child failed\" }\n",
    "      fold = mkFold [mkEntry 1 \"split\" leafBranch splitPayload, mkEntry 2 \"replan\" leafBranch (toJSON decision)]\n",
    "      expected = ResumeAmend decision (amendPlan decision splitPlan)\n",
    "  in verify \"replan-newer-than-split-amends\" (resumePlanFor fold leafBranch leafPlan == expected)\n",
    "\n",
    "-- A replan OLDER than the split it would amend does not: the split still\n",
    "-- replays under its ORIGINAL recorded plan, and the stale replan plays no\n",
    "-- part in the decision at all.\n",
    "caseReplanOlderIgnored :: Text\n",
    "caseReplanOlderIgnored =\n",
    "  let splitPlan = leafPlan { nodeTask = \"still the original task\" }\n",
    "      splitPayload = splitPayloadFor \"leaf\" splitPlan \"scaffold2\"\n",
    "      decision = ReplanDecision { amendedInstruction = \"a stale amendment\", abandonSubtree = False, rationale = \"stale\" }\n",
    "      fold = mkFold [mkEntry 2 \"split\" leafBranch splitPayload, mkEntry 1 \"replan\" leafBranch (toJSON decision)]\n",
    "      expected = ResumeReplay SplitRecord { splitNode = \"leaf\", splitScaffoldHead = \"scaffold2\", splitPlan = splitPlan, splitChildTrees = [] }\n",
    "  in verify \"replan-older-than-split-is-ignored\" (resumePlanFor fold leafBranch leafPlan == expected)\n",
    "\n",
    "-- The empty fold. 'resumed' is 'id' when 'isResumed' is False, which is the\n",
    "-- mechanism that makes 'resumeLoop emptyResume' re-enter the SAME coalgebra\n",
    "-- 'loop' does rather than a second one that could drift from it; this checks\n",
    "-- that gate directly, plus the per-branch decision this fold would reach if\n",
    "-- ever consulted.\n",
    "caseEmptyFoldIsResumedFalse :: Text\n",
    "caseEmptyFoldIsResumedFalse = verify \"empty-fold-isResumed-false\" (isResumed emptyResume == False)\n",
    "\n",
    "caseEmptyFoldDecidesFresh :: Text\n",
    "caseEmptyFoldDecidesFresh = verify \"empty-fold-resumePlanFor-is-fresh\" (resumePlanFor emptyResume leafBranch leafPlan == ResumeFresh)\n",
    "\n",
    "-- Entries recorded under an UNRELATED branch do not leak into this branch's\n",
    "-- decision: ordinary work, not accidental skipping.\n",
    "caseUnrelatedBranchIsFresh :: Text\n",
    "caseUnrelatedBranchIsFresh =\n",
    "  let otherBranch = \"dev-tree/other\"\n",
    "      fold = mkFold [mkEntry 1 \"outcome\" otherBranch (toJSON (mkReceipt \"other\" otherBranch))]\n",
    "  in verify \"unrelated-branch-entries-do-not-skip\" (resumePlanFor fold leafBranch leafPlan == ResumeFresh)\n",
    "\n",
    "-- amendmentIsNewest, directly: the precedence rule every case above rests on.\n",
    "caseAmendmentIsNewest :: [Text]\n",
    "caseAmendmentIsNewest =\n",
    "  [ verify \"amendmentIsNewest-no-replan-is-false\" (amendmentIsNewest Nothing (Just 5) (Just 3) == False)\n",
    "  , verify \"amendmentIsNewest-replan-newer-than-both\" (amendmentIsNewest (Just 4) (Just 2) (Just 1) == True)\n",
    "  , verify \"amendmentIsNewest-replan-equal-to-split-not-newer\" (amendmentIsNewest (Just 2) (Just 2) Nothing == False)\n",
    "  , verify \"amendmentIsNewest-replan-newer-than-split-older-than-outcome\" (amendmentIsNewest (Just 3) (Just 1) (Just 5) == False)\n",
    "  , verify \"amendmentIsNewest-replan-with-no-prior-split-or-outcome\" (amendmentIsNewest (Just 0) Nothing Nothing == True)\n",
    "  ]\n",
    "\n",
    "__resumeDecisionReport :: Text\n",
    "__resumeDecisionReport = T.intercalate \"\\n\" ( [ caseRecordedOutcome, caseRecordedSplitReplays, caseReplanNewerAmends, caseReplanOlderIgnored, caseEmptyFoldIsResumedFalse, caseEmptyFoldDecidesFresh, caseUnrelatedBranchIsFresh ] <> caseAmendmentIsNewest )\n",
);

/// dev-tree's resume decisions, EXECUTED — not merely compiled.
///
/// `dev_tree_typechecks` above proves this module still compiles against
/// today's effect surface, including `resumeLoop` and the pure resume
/// decisions at their exact declared signatures — but it never calls them, so
/// a wrong PRECEDENCE (e.g. an older replan winning, or a recorded split
/// being silently re-derived instead of replayed) would compile clean and
/// only surface as corrupted state in a real crash-resume run. This is the
/// next rung: it builds `ResumeFold` values by hand — a recorded outcome, a
/// recorded split with no outcome, a replan newer than its split, a replan
/// older than its split, the empty fold, and entries recorded under an
/// unrelated branch — runs `resumePlanFor`/`amendmentIsNewest` over them on
/// the real JIT with no agent and no git anywhere in the path, and asserts
/// what comes back against the decision table read directly out of
/// `Harness.hs`.
#[test]
fn dev_tree_resume_decisions_execute() {
    let json = execute_pure(
        "harness-dogfooding/dev-tree",
        dev_tree_decls(),
        RESUME_DECISION_SOURCE,
        "__resumeDecisionReport",
    );
    let report = json
        .as_str()
        .expect("__resumeDecisionReport :: Text renders as a JSON string");
    let lines: Vec<&str> = report.lines().collect();
    let failures: Vec<&&str> = lines.iter().filter(|l| l.starts_with("FAIL")).collect();
    assert!(
        failures.is_empty(),
        "resume decision(s) diverged from the spec:\n{report}"
    );
    assert_eq!(
        lines.len(),
        12,
        "expected 12 resume-decision checks, got:\n{report}"
    );
}

/// The overspend regression the external review flagged
/// (`harness-dogfooding/dev-tree/Harness.hs:420-429`): `childAllowance`'s old
/// `max 1 ((parent.seedCycles - 2) \`div\` n)` minted a cycle from nothing
/// whenever the floor share rounded to zero. A non-leaf root with
/// `seedCycles = 2` and two leaf children passes `cycleRefusal` itself (its
/// own reservation is exactly met), but the OLD formula then handed each
/// child 1 cycle anyway — 2 (parent) + 1 + 1 = 4, double the parent's own
/// cap of 2.
///
/// The fix removes the upward clamp (`max 0` instead of `max 1`), so the same
/// scenario floors each child's share to 0; `requiredCycles` (1 for a leaf)
/// then exceeds that share, which is exactly the typed budget refusal
/// `cycleRefusal` already provides — no child spends anything. This pins
/// total spend at <= the parent's cap using ONLY `childAllowance` and
/// `requiredCycles` (both pure, no agent, no git), mirroring how
/// `cycleRefusal`/`allocateChildren` actually combine them in `decompose`.
const OVERSPEND_REGRESSION_SOURCE: &str = concat!(
    "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, ",
    "FlexibleContexts, GADTs, ScopedTypeVariables, TypeApplications, LambdaCase, ",
    "RecordWildCards, OverloadedRecordDot, QuasiQuotes, DeriveGeneric, DeriveAnyClass #-}\n",
    "module OverspendProbe where\n",
    "import Tidepool.Prelude hiding (render)\n",
    "import Tidepool.Effects\n",
    "import Harness\n",
    "import HarnessTypes (OnFailure (..))\n",
    "import Tidepool.QQ (fmt)\n",
    "import qualified Data.Text as T\n",
    "\n",
    "leafPlan :: DevPlan\n",
    "leafPlan = DevPlan { nodeName = \"leaf\", nodeTask = \"t\", nodeChecks = [], nodeBoundary = [], nodeOnFailure = Retry, childPlans = [] }\n",
    "\n",
    "rootPlan :: DevPlan\n",
    "rootPlan = DevPlan { nodeName = \"root\", nodeTask = \"t\", nodeChecks = [], nodeBoundary = [], nodeOnFailure = Retry, childPlans = [leafPlan, leafPlan] }\n",
    "\n",
    "-- Never forced: 'childAllowance'/'requiredCycles' only read 'seedPlan' and\n",
    "-- 'seedCycles', so a live 'WorktreeHandle' is not needed for this pin.\n",
    "rootSeed :: NodeSeed\n",
    "rootSeed = NodeSeed { seedPlan = rootPlan, seedTree = undefined, seedDepth = 0, seedCycles = 2, seedAdopted = Nothing }\n",
    "\n",
    "nChildren :: Int\n",
    "nChildren = length (childPlans rootPlan)\n",
    "\n",
    "childShare :: Int\n",
    "childShare = childAllowance rootSeed nChildren\n",
    "\n",
    "-- A child only spends its share if it clears 'cycleRefusal' (the same test\n",
    "-- that function runs: seedCycles >= requiredCycles seedPlan). Below that\n",
    "-- threshold the child is refused and spends 0 — never the share itself.\n",
    "childSpend :: Int\n",
    "childSpend = if childShare >= requiredCycles leafPlan then childShare else 0\n",
    "\n",
    "totalSpend :: Int\n",
    "totalSpend = requiredCycles rootPlan + nChildren * childSpend\n",
    "\n",
    "__overspendReport :: Text\n",
    "__overspendReport =\n",
    "  T.intercalate \"\\n\"\n",
    "    [ [fmt|childShare={childShare}|]\n",
    "    , [fmt|totalSpend={totalSpend}|]\n",
    "    , [fmt|withinCap={totalSpend <= rootSeed.seedCycles}|]\n",
    "    ]\n",
);

/// Runs [`OVERSPEND_REGRESSION_SOURCE`] on the real JIT (pure, no agent, no
/// git) and asserts the exact numbers the review's scenario names: a floored
/// share of 0 per child, and total spend pinned at the parent's own
/// reservation (2) — never 4, which is what the pre-fix `max 1` formula
/// would have produced.
#[test]
fn dev_tree_child_allowance_never_overspends_parent_cap() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    let cfg = EngineConfig::from_decls(
        dev_tree_decls(),
        repo_root().join("haskell/lib"),
        Some(repo_root().join("harness-dogfooding/dev-tree")),
    )
    .expect("engine config for the dogfood row");
    let include: Vec<_> = cfg.include.iter().map(|p| p.as_path()).collect();
    let json =
        match compile_and_run_pure(OVERSPEND_REGRESSION_SOURCE, "__overspendReport", &include) {
            Ok(result) => result.to_json(),
            Err(e) => panic!("overspend regression probe did not run cleanly:\n{e}"),
        };
    let report = json
        .as_str()
        .expect("__overspendReport :: Text renders as a JSON string");
    assert!(report.contains("childShare=0"), "{report}");
    assert!(report.contains("totalSpend=2"), "{report}");
    assert!(report.contains("withinCap=True"), "{report}");
}
