//! PRD 21 lane C3 GAP 1 — the frozen-snapshot seam's authored-surface reach:
//! `freezeContext`/`runLLMTurnBranch` (`SelfHarnessDriver::service_outer_branch`),
//! through the AUTHORED outer loop rather than a test-only `Harness` call.
//!
//! Joins two things `tests/companion_snapshots.rs` and
//! `tests/companion_scope_trees.rs` each pin at their own layer, THROUGH the
//! new authored verbs:
//!
//! - **A real fork off the frozen prefix, never an empty root.** Before this
//!   lane, every `runLLMTurnFork`/`runLLMTurnFanout` child from the AUTHORED
//!   loop opened via `create_root_framed(_, "", _)` — an EMPTY root, framing
//!   only. `Event::BranchInvocation` is written ONLY for a node minted
//!   through `fork_from_snapshot`/`fork_from_context_ref` (and only after the
//!   harness's OWN internal re-digest-and-compare check passes — see
//!   `Harness::log_branch_invocation`'s doc); an empty-root child could never
//!   produce one. Its existence, naming the SAME digest `freezeContext`
//!   minted, with `shared_prefix_bytes` equal to that root's own byte count,
//!   IS the byte-stability proof — re-derived by the harness itself, not
//!   re-guessed here.
//! - **Cross-window declaration inheritance (C2 §1–3), through a branched
//!   tree.** `runLLMTurnBranch`'s child scope is minted as a child of the
//!   frozen window's own scope (`Harness::context_ref_scope`), not left at
//!   the flat `ScopeId::ROOT` every fanout/fork child defaults to — the same
//!   upward-visible/downward-invisible walk `companion_scope_trees.rs`'s
//!   `locked_decision_4_holds_through_the_real_compile_path` proves for an
//!   ordinary `mint_scope` tree, proved here for a BRANCHED one instead.
//! - **A branch child's abnormal exit is DATA at its branch position** (PRD
//!   21 locked decision 6): `runLLMTurnBranch @T` answers
//!   `Either InvocationExit (T, ContextRef)`, so a branch whose window
//!   exhausts its rounds folds as `Left` there instead of aborting the outer
//!   turn and erasing its sibling's finished answer.
//!
//! ONE fixture (`fixtures/ContextRefHarness.hs`), ONE compile shape shared by
//! both tests below — family-bundle discipline.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    repo_root().join("tidepool-harness/tests/fixtures")
}

fn header(label: &str) -> LogHeader {
    LogHeader {
        prelude_hash: format!("context-ref-{label}"),
        extract_fingerprint: format!("context-ref-{label}"),
        harness_version: "test".into(),
    }
}

/// A scripted reply carrying one fenced Haskell block — order matters:
/// [`ReplayProvider`] serves these strictly FIFO, which is exactly right
/// here since nothing in this fixture's run is concurrent (branch calls are
/// sequential suspend/resume round-trips, one per `runLLMTurnBranch`).
fn reply(block: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: format!("```haskell\n{block}\n```"),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
        },
    }
}

fn events(path: &std::path::Path) -> Vec<Event> {
    let (_header, iter) = LogReader::open(path).expect("log opens");
    iter.map(|r| r.expect("event parses").event).collect()
}

/// The fixture's two per-branch projections: `answers` (the branch answers
/// that arrived, plus ROOT's own trailing re-read) and `outcomes` (one entry
/// per BRANCH POSITION — `ok:<n>` or `exit:<reason>`).
fn answers_and_outcomes(state_json: &serde_json::Value) -> (Vec<i64>, Vec<String>) {
    let answers = state_json
        .get("answers")
        .and_then(|v| v.as_array())
        .expect("answers is a JSON array")
        .iter()
        .map(|v| v.as_i64().expect("each answer is an Int"))
        .collect();
    let outcomes = state_json
        .get("outcomes")
        .and_then(|v| v.as_array())
        .expect("outcomes is a JSON array")
        .iter()
        .map(|v| v.as_str().expect("each outcome is Text").to_string())
        .collect();
    (answers, outcomes)
}

/// The six scripted replies, in the exact order `ContextRefHarness.hs`'s
/// `loop` drives them:
///
/// 1. ROOT declares `helper x = x + 100` (a DEFINE round — `Completed`, not
///    `finalize`, so the driver auto-reprompts on the SAME hole).
/// 2. ROOT finalizes the `runLLMTurn @Bool` hole.
/// 3. Branch A finalizes immediately, reading ROOT's `helper` UNCHANGED.
/// 4. Branch B defines its OWN local `helper x = x * 2` (shadowing freely —
///    another DEFINE round on a FRESH child node).
/// 5. Branch B finalizes, using ITS OWN local `helper`.
/// 6. ROOT's second `runLLMTurn @Int` hole re-reads `helper` — must still be
///    the ORIGINAL (locked decision 4: the parent never gains a child's
///    declarations).
fn scripted_replies() -> Vec<RecordedReply> {
    vec![
        reply("helper :: Int -> Int\nhelper x = x + 100"),
        reply("finalize @Bool True"),
        reply("finalize @Int (helper 1)"),
        reply("helper :: Int -> Int\nhelper x = x * 2"),
        reply("finalize @Int (helper 1)"),
        reply("finalize @Int (helper 1)"),
    ]
}

/// The end-to-end acceptance: `runLLMTurnBranch` forks two REAL children off
/// one `freezeContext` ref, each inheriting ROOT's declarations while its own
/// stays local, and ROOT is untouched by either afterward.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_forks_from_frozen_prefix_and_inherits_declarations() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(scripted_replies()));
    let log_path =
        std::env::temp_dir().join(format!("context-ref-cycle-{}.jsonl", std::process::id()));
    let writer = LogWriter::create(&log_path, &header("cycle")).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent.clone(), Arc::new(LogObserver));

    let source = load_harness_source(&fixtures_dir().join("ContextRefHarness.hs"))
        .expect("context-ref fixture loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one render->loop->freezeContext->runLLMTurnBranch(x2)->finalize->render cycle");

    // --- decl-scope inheritance, proved by VALUE (C2's scope contract) ---
    let (answers, outcomes) = answers_and_outcomes(&outcome.state_json);
    assert_eq!(
        outcomes,
        vec!["ok:101".to_string(), "ok:2".to_string()],
        "both branch positions carry their own answer in this scenario"
    );
    assert_eq!(
        answers,
        vec![101, 2, 101],
        "branch A must read ROOT's shared `helper` unmodified (101); branch B's \
         own local redefinition must stay scoped to B alone (2); and ROOT's \
         `helper` must be UNCHANGED after both branches finish (101, not 2) — \
         locked decision 4's upward-visible/downward-invisible walk, now proved \
         through runLLMTurnBranch rather than a bare mint_scope tree"
    );

    // --- a real fork off the frozen prefix, never an empty root ---
    let log = events(&log_path);

    // THREE distinct windows freeze in this run: the ROOT window (once, via
    // freezeContext) plus each branch child's OWN post-finalize freeze
    // (`service_outer_branch` mints the returned `ContextRef` from the
    // child's own frozen prefix, so it can be branched further) — three
    // `SnapshotFrozen` receipts, not one.
    let frozen: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Event::SnapshotFrozen {
                digest,
                prefix_bytes,
                ..
            } => Some((digest.clone(), *prefix_bytes)),
            _ => None,
        })
        .collect();
    assert_eq!(
        frozen.len(),
        3,
        "three windows freeze once each: the ROOT window (freezeContext) and \
         each branch child's own post-finalize freeze (its returned ContextRef)"
    );

    let branches: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Event::BranchInvocation {
                node,
                snapshot,
                shared_prefix_bytes,
                branch_suffix_bytes,
                ..
            } => Some((
                *node,
                snapshot.clone(),
                *shared_prefix_bytes,
                *branch_suffix_bytes,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        branches.len(),
        2,
        "TWO runLLMTurnBranch children, each a REAL fork_from_snapshot child — \
         an empty-root child (the pre-fix GAP 1 bug) writes NO BranchInvocation \
         receipt at all, so this count alone regression-guards the fix"
    );
    let distinct_nodes: HashSet<NodeId> = branches.iter().map(|(n, ..)| *n).collect();
    assert_eq!(
        distinct_nodes.len(),
        2,
        "the two branches must be genuinely DISTINCT child nodes"
    );

    // Both branches must name the SAME parent digest — the ROOT window's
    // freezeContext root, shared byte-stably by both siblings.
    let parent_digests: HashSet<_> = branches.iter().map(|(_, snapshot, ..)| snapshot).collect();
    assert_eq!(
        parent_digests.len(),
        1,
        "both branches must share ONE frozen root — every sibling branch \
         names the SAME freezeContext digest"
    );
    let parent_digest = (*parent_digests.into_iter().next().unwrap()).clone();
    let (_, parent_prefix_bytes) = frozen
        .iter()
        .find(|(d, _)| *d == parent_digest)
        .cloned()
        .expect("the shared parent digest must itself have a SnapshotFrozen receipt");
    assert!(
        parent_prefix_bytes > 0,
        "the frozen root prefix must be non-empty"
    );

    for (node, snapshot, shared, suffix) in &branches {
        assert_eq!(
            *snapshot, parent_digest,
            "branch {node:?} must name the SAME frozen root freezeContext minted"
        );
        assert_eq!(
            *shared, parent_prefix_bytes,
            "branch {node:?}'s shared-prefix byte count must equal the frozen \
             root's own — this receipt is written ONLY after the harness's own \
             re-digest-and-compare check passed (Harness::log_branch_invocation), \
             so equality here is the byte-stability proof, not a re-guess of it"
        );
        assert!(
            *suffix > 0,
            "branch {node:?} must carry its own hole-card suffix past the shared prefix"
        );
    }
}

/// PRD 21 locked decision 6, applied to the BRANCH verb: branch A's window
/// exhausts its rounds while branch B finalizes normally. The outer turn must
/// still complete, branch B's answer and ROOT's own trailing re-read must both
/// arrive, and branch A must arrive as a typed `InvocationExit` at its own
/// branch position — not as an error that takes the turn (and B's finished
/// answer) down with it.
///
/// `Either InvocationExit (T, ContextRef)`, not
/// `(Either InvocationExit T, ContextRef)`: a window that never finalized has
/// no post-finalize prefix, so there is no honest `ContextRef` to sit beside
/// the failure — the `Either` wraps the whole pair.
///
/// Round caps are lowered to 1/2 (hard stop at max+2 = 4 rounds) and branch
/// A's replies carry no ```haskell block, so its budget is spent in four
/// instant `NoBlock` re-prompts with nothing compiled on that branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_child_that_exhausts_its_rounds_folds_as_data_without_erasing_its_sibling() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let prose = |text: &str| RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: text.to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
        },
    };
    let replies = vec![
        // ROOT's `runLLMTurn @Bool` hole: define, then finalize.
        reply("helper :: Int -> Int\nhelper x = x + 100"),
        reply("finalize @Bool True"),
        // Branch A: four block-less rounds — nudge, ultimatum, and the two
        // grace rounds — then the window is out of budget.
        prose("Branch A: still thinking (1)."),
        prose("Branch A: still thinking (2)."),
        prose("Branch A: still thinking (3)."),
        prose("Branch A: still thinking (4)."),
        // Branch B: unaffected, defines its own helper and finalizes.
        reply("helper :: Int -> Int\nhelper x = x * 2"),
        reply("finalize @Int (helper 1)"),
        // ROOT's second hole: `helper` is still ROOT's own.
        reply("finalize @Int (helper 1)"),
    ];

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let log_path =
        std::env::temp_dir().join(format!("context-ref-exit-{}.jsonl", std::process::id()));
    let writer = LogWriter::create(&log_path, &header("exit")).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_answerer_round_caps(1, 2);

    let source = load_harness_source(&fixtures_dir().join("ContextRefHarness.hs"))
        .expect("context-ref fixture loads");

    // The turn COMPLETES. Before decision 6 reached this verb, branch A's
    // round exhaustion propagated out of `service_outer_branch` and this call
    // returned `Err`.
    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("the outer turn completes despite branch A's window exiting");

    let (answers, outcomes) = answers_and_outcomes(&outcome.state_json);
    assert_eq!(
        answers,
        vec![2, 101],
        "branch B's answer (2) and ROOT's own trailing re-read (101) must both \
         survive branch A's exit; branch A contributes nothing rather than \
         displacing anyone"
    );
    assert_eq!(outcomes.len(), 2, "one outcome per branch position");
    assert_eq!(
        outcomes[0],
        "exit:round exhaustion: runLLMTurn answerer exceeded 4 rounds (cap 2 + \
         ultimatum grace) without finalizing",
        "branch A's position carries the typed round-exhaustion exit, rendered \
         by `renderInvocationExit`"
    );
    assert_eq!(
        outcomes[1], "ok:2",
        "branch B's position is untouched by its sibling's failure"
    );

    // The exited branch left no BranchInvocation-less debris: branch B still
    // forked off the SAME frozen root, so the seam itself is unaffected.
    let branches = events(&log_path)
        .into_iter()
        .filter(|e| matches!(e, Event::BranchInvocation { .. }))
        .count();
    assert_eq!(
        branches, 2,
        "BOTH branch children are still real fork_from_snapshot children — an \
         exiting window is one that failed to ANSWER, not one that was never opened"
    );
}
