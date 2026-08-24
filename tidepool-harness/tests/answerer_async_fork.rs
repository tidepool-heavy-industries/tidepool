//! Acceptance coverage for the ANSWERER-PLANE green scheduler
//! (`SelfHarnessDriver::service_green_round`): the composed idiom
//! `async (fork @T brief)` — a model window spawning several fork children
//! as green threads, `wait`-ing their typed results, and finalizing from
//! them — plus the per-window fork budget's loud refusal path. Driven
//! through the production entry point (`SelfHarnessDriver::run_one_loop_iteration`)
//! against the reference harness (`examples/harness/Harness.hs`), scripted
//! record-replay, zero live calls — the same discipline as
//! `selfharness_spine.rs`.
//!
//! The ReplayProvider queue is itself a behavior pin in both tests: the
//! composition test's queue only works if BOTH children are driven (in
//! spawn order) after the one spawning block; the budget test's queue has
//! NO second-child reply, so a refusal that failed to refuse would consume
//! the recovery finalize as the second child's turn and fail loudly.
//!
//! Also carries the fork child's operator-GUI/tree lifecycle pin (folded in
//! from the retired `fork_child_gui.rs`, test-architecture review W1,
//! 2026-08-23 — a full driver-boot twin of this file's own shape, saving a
//! separate binary): a fork child gets the SAME `node_gate`/`node_seeded`/
//! `node_finalized`/`retire_node` treatment a `runLLMTurnBranchLabeled`
//! child gets, even though nothing on the wire hands it a label.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

mod support;

use serde_json::json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::operator::{FormShape, OperatorGate};
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Harness, LogObserver, SelfHarnessDriver,
};

fn repo_root() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> std::path::PathBuf {
    repo_root().join("haskell/lib")
}

fn examples_harness_dir() -> std::path::PathBuf {
    repo_root().join("examples/harness")
}

fn fixtures_dir() -> std::path::PathBuf {
    repo_root().join("tidepool-harness/tests/fixtures")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "answerer-async-fork".into(),
        extract_fingerprint: "answerer-async-fork".into(),
        harness_version: "test".into(),
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        content: content.to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

/// The answerer block under test: two forks spawned as green threads, BOTH
/// outstanding before the first `wait`, results combined into the finalize.
/// `a * 10 + b` pins that each child's value landed on the right handle —
/// a swapped delivery yields 21, not 12.
// NOTE: single-line literal with explicit \n — a `\` line-continuation
// strips the next line's leading spaces, which silently destroys do-block
// indentation (a GHC parse error that sends every test down the corrective
// path; caught live on this file's first run).
const ASYNC_FORK_BLOCK: &str = "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\nimport Tidepool.Fork (fork)\n\ndo\n  ha <- async (fork @Int \"pick a\")\n  hb <- async (fork @Int \"pick b\")\n  a <- wait ha\n  b <- wait hb\n  (finalize @Decision (Decision { action = show (a * 10 + b), rationale = \"async fork composition\", confidence = Medium }) :: M ())\n```";

// Fork children are full pump windows (fork-subsumes-split step 1): they
// answer with a REAL `finalize @Int`, pinned by the fork site's contract.
fn finalize_int_reply(n: i64) -> RecordedReply {
    reply(&format!(
        "```haskell\n(finalize @Int ({n} :: Int) :: M ())\n```"
    ))
}

fn build_driver(
    replies: Vec<RecordedReply>,
    label: &str,
) -> (SelfHarnessDriver, Arc<Harness>, std::path::PathBuf) {
    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let log_path = std::env::temp_dir().join(format!("{label}-{}.jsonl", std::process::id()));
    let writer =
        tidepool_harness::log::LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    (
        SelfHarnessDriver::new(agent.clone(), Arc::new(LogObserver)),
        agent,
        log_path,
    )
}

/// Every logged turn's text (user + assistant), for asserting a specific
/// corrective actually reached the model — the assertion that keeps a
/// compile-error corrective loop from impersonating the path under test
/// (this file's first run did exactly that: a de-indented do-block never
/// compiled, and the budget test "passed" without ever forking).
fn logged_turn_texts(log_path: &std::path::Path) -> Vec<String> {
    let (_header, events) =
        tidepool_harness::log::LogReader::open(log_path).expect("open test log");
    events
        .filter_map(|r| r.ok())
        .filter_map(|r| match r.event {
            tidepool_harness::log::Event::TurnDelta { content, .. } => Some(content),
            _ => None,
        })
        .collect()
}

/// The composed idiom end to end: `async (fork @Int …)` twice, `wait` twice,
/// finalize from both results — one block, one round. The scheduler must
/// spawn BOTH children from the one block (both briefs captured before
/// either child runs a turn), drive them in spawn order against the shared
/// machine, deliver each typed result to the RIGHT handle, and let the
/// window finalize with the combination: action = "12" (1×10 + 2), never
/// "21" (swapped) or a starved/hung scheduler.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_fork_composition_two_children_typed_results_cross() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // 1. The answerer's single spawning block.
        reply(ASYNC_FORK_BLOCK),
        // 2. Fork child A ("pick a") — driven first (spawn order).
        finalize_int_reply(1),
        // 3. Fork child B ("pick b").
        finalize_int_reply(2),
    ];
    let (mut driver, _agent, _log_path) = build_driver(replies, "answerer-async-fork");
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("one cycle: spawn two async forks, wait both, finalize");

    let state = &outcome.state_json;
    let decision = state
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision, not null");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("12"),
        "each child's typed Int must land on the RIGHT handle (1×10 + 2 = 12; a \
         swapped delivery reads 21), got {decision:?}"
    );
}

/// MULTI-WAVE: fork, fold, fork again within ONE block — wave 2's brief is
/// COMPUTED from wave 1's results (`a + b = 3` folded in ordinary Haskell,
/// spliced into the second wave's brief text), and the final answer carries
/// wave 2's result. This is the property that would silently break if
/// scheduler state ever confused join generations: the wave-2 child's brief
/// must contain wave 1's ACTUAL folded value, proving the dataflow crosses
/// waves, not just that three children ran. (The brief-content route is
/// unobservable here — fork-child hole cards ride the forked-transcript
/// path, not the TurnDelta stream — so the proof is arithmetic: the final
/// action `show (c + s)` reads 13 only if wave 1's fold reached the code
/// after wave 2.)
// Same single-line-literal discipline as ASYNC_FORK_BLOCK (see its note).
const TWO_WAVE_BLOCK: &str = "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\nimport Tidepool.Fork (fork)\n\ndo\n  ha <- async (fork @Int \"pick a\")\n  hb <- async (fork @Int \"pick b\")\n  a <- wait ha\n  b <- wait hb\n  let s = a + b\n  hc <- async (fork @Int (\"wave two, given \" <> show s))\n  c <- wait hc\n  (finalize @Decision (Decision { action = show (c + s), rationale = \"two waves\", confidence = Medium }) :: M ())\n```";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_waves_of_fork_fold_fork_carry_results_across_waves() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // 1. The two-wave block.
        reply(TWO_WAVE_BLOCK),
        // 2-3. Wave 1's children, in spawn order.
        finalize_int_reply(1),
        finalize_int_reply(2),
        // 4. Wave 2's one child: a STATIC 10 — the final "13" is only
        //    reachable through the parent's own `c + s`, so it proves the
        //    wave-1 fold (s = 3) survived into the code after wave 2.
        finalize_int_reply(10),
    ];
    let (mut driver, _agent, log_path) = build_driver(replies, "answerer-async-waves");
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("two waves of fork/fold/fork complete in one window");

    let state = &outcome.state_json;
    let decision = state
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision, not null");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("13"),
        "10 (wave 2's static reply) + 3 (wave 1's fold) = 13 — anything else \
         means values crossed to the wrong handles or a wave's results were \
         lost, got {decision:?}"
    );

    let turns = logged_turn_texts(&log_path);
    assert!(
        !turns.iter().any(|t| t.contains("A block did not compile")),
        "no turn may be a GHC corrective; logged turns:\n{turns:#?}"
    );
}

/// DEPTH CONTAINMENT (step 1½): a fork CHILD that itself tries to fork is
/// refused loudly — its block aborts, the corrective names the boundary,
/// and the child recovers by answering its own brief directly. Guards the
/// gap the pump migration opened: children compile against the full
/// answerer effect list now (the old fork-free child row is gone), so
/// without this bound a child could fork 32-wide recursively with no depth
/// cap until step 2's subtree budgets land.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_child_that_forks_is_depth_refused_and_recovers() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // 1. Parent: one direct fork, answer derived from the child's value.
        reply(
            "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\nimport Tidepool.Fork (fork)\n\ndo\n  a <- fork @Int \"pick a\"\n  (finalize @Decision (Decision { action = show a, rationale = \"one child\", confidence = Medium }) :: M ())\n```",
        ),
        // 2. The child's FIRST attempt: tries to fork a grandchild. This
        //    compiles (Fork is in the row) and must be refused at servicing.
        reply(
            "```haskell\nimport Tidepool.Fork (fork)\n\ndo\n  b <- fork @Int \"grandchild\"\n  (finalize @Int (b + 1) :: M ())\n```",
        ),
        // 3. The child's recovery after the depth-refusal corrective.
        finalize_int_reply(7),
    ];
    let (mut driver, _agent, log_path) = build_driver(replies, "answerer-fork-depth");
    // Step 2 raised the default depth cap to 8; this test pins the refusal
    // MECHANISM at depth 1 rather than scripting 8 nested sessions.
    driver.set_max_fork_depth(1);
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("the depth refusal aborts the child's block, not the run");

    let decision = outcome
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("7"),
        "the parent must receive the child's RECOVERY answer, got {decision:?}"
    );
    let turns = logged_turn_texts(&log_path);
    assert!(
        turns
            .iter()
            .any(|t| t.contains("Forking is not available in THIS session")),
        "the depth-refusal corrective must reach the child; logged turns:\n{turns:#?}"
    );
}

/// STEP 2, the raise: with the default caps (depth 8, subtree 32), a fork
/// CHILD can itself fork — a two-level chain runs end to end and values
/// flow up through both finalizes: grandchild 5 → child 5+1 → parent "6".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_level_fork_chain_succeeds_at_default_caps() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        reply(
            "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\nimport Tidepool.Fork (fork)\n\ndo\n  a <- fork @Int \"pick a\"\n  (finalize @Decision (Decision { action = show a, rationale = \"chain\", confidence = Medium }) :: M ())\n```",
        ),
        // The child forks a grandchild and derives its own answer from it.
        reply(
            "```haskell\nimport Tidepool.Fork (fork)\n\ndo\n  b <- fork @Int \"grandchild\"\n  (finalize @Int (b + 1) :: M ())\n```",
        ),
        finalize_int_reply(5),
    ];
    let (mut driver, _agent, _log_path) = build_driver(replies, "answerer-fork-chain");
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("a depth-2 fork chain completes at the default caps");

    let decision = outcome
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("6"),
        "the grandchild's 5 must flow up through the child's +1 into the \
         parent's answer, got {decision:?}"
    );
}

/// STEP 2, the tree-wide bound: with a subtree cap of 1, the block's SECOND
/// fork is refused with the WHOLE-TREE corrective (distinct from the
/// per-session pool's), the block aborts, and the session recovers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_fork_past_subtree_cap_refuses_with_tree_wide_corrective() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        reply(
            "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\nimport Tidepool.Fork (fork)\n\ndo\n  a <- fork @Int \"pick a\"\n  b <- fork @Int \"pick b\"\n  (finalize @Decision (Decision { action = show (a + b), rationale = \"two\", confidence = Medium }) :: M ())\n```",
        ),
        finalize_int_reply(1),
        reply(
            "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n(finalize @Decision (Decision { action = \"gave-up\", rationale = \"subtree cap\", confidence = Medium }) :: M ())\n```",
        ),
    ];
    let (mut driver, _agent, log_path) = build_driver(replies, "answerer-fork-subtree");
    driver.set_fork_subtree_cap(1);
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("the subtree refusal aborts the block, not the run");

    let decision = outcome
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("gave-up")
    );
    let turns = logged_turn_texts(&log_path);
    assert!(
        turns
            .iter()
            .any(|t| t.contains("Fork budget exhausted for this WHOLE tree")),
        "the tree-wide corrective must reach the model; logged turns:\n{turns:#?}"
    );
}

/// F8 regression (driver structural review, 2026-08-23): the SAME tree-wide
/// subtree refusal, but through the ASYNC green-thread fork path
/// (`async (fork @T …)`, serviced by `service_thread_ready`'s Fork arm)
/// rather than a direct `fork`. Before the fix, that arm discarded
/// `check_fork_budgets`' real message and the dispatcher unconditionally
/// rebuilt a PER-WINDOW `fork_budget_refusal` — so a subtree exhaustion here
/// was misreported with per-window wording/numbers ("this session has
/// spawned N of its CAP fork children"), contradicting the design intent
/// that the two refusals stay textually distinguishable so the model learns
/// the boundary is tree-wide, not something a deeper/sibling fork escapes.
/// With `fork_subtree_cap` at 1, thread "pick a"'s own fork spends the
/// tree's only slot; thread "pick b"'s fork then hits the ALREADY-exhausted
/// subtree cap (not its own per-window pool, which still has room) — the
/// corrective MUST read as the tree-wide message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_fork_over_subtree_cap_refuses_with_tree_wide_corrective() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // 1. The same two-async-fork spawning block — "pick b"'s fork must
        //    refuse against the tree-wide cap, not the per-window pool.
        reply(ASYNC_FORK_BLOCK),
        // 2. "pick a"'s fork child — the one child the subtree cap covers.
        finalize_int_reply(1),
        // 3. The corrective round after the refusal: finalize plainly.
        reply(
            "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
             (finalize @Decision (Decision { action = \"gave-up\", rationale = \"subtree \
             cap via async fork\", confidence = Medium }) :: M ())\n```",
        ),
    ];
    let (mut driver, _agent, log_path) = build_driver(replies, "answerer-async-subtree");
    driver.set_fork_subtree_cap(1);
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("the subtree refusal aborts the block, not the window — the cycle completes");

    let decision = outcome
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("gave-up"),
        "the window must survive the refusal and finalize on its next round, \
         got {decision:?}"
    );

    let turns = logged_turn_texts(&log_path);
    assert!(
        turns
            .iter()
            .any(|t| t.contains("Fork budget exhausted for this WHOLE tree")),
        "an async-fork subtree refusal must carry the TREE-WIDE corrective, not a \
         rebuilt per-window one; logged turns:\n{turns:#?}"
    );
    assert!(
        !turns.iter().any(|t| t.contains("this session has spawned")),
        "the per-window refusal wording must NOT appear — that would mean the \
         real subtree message was discarded and rebuilt wrong (F8); logged \
         turns:\n{turns:#?}"
    );
}

/// The fork budget's loud refusal: with a budget of 1, the block's SECOND
/// async fork is refused — the block is aborted (its first result is lost
/// with it), the round's threads are swept, the corrective names the
/// budget, and the WINDOW survives to finalize plainly on its next round.
/// The reply queue pins the mechanism: there is no second-child reply, so a
/// refusal that spawned anyway would consume the recovery finalize as the
/// second child's turn and fail the queue loudly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_fork_over_budget_refuses_loudly_and_window_survives() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // 1. The same two-fork spawning block — the second fork must refuse.
        reply(ASYNC_FORK_BLOCK),
        // 2. Fork child A ("pick a") — the one child the budget covers.
        finalize_int_reply(1),
        // 3. The corrective round after the refusal: finalize plainly.
        reply(
            "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
             (finalize @Decision (Decision { action = \"gave-up\", rationale = \"fork \
             budget refused\", confidence = Medium }) :: M ())\n```",
        ),
    ];
    let (mut driver, _agent, log_path) = build_driver(replies, "answerer-async-budget");
    driver.set_fork_budget_per_window(1);
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("the refusal aborts the block, not the window — the cycle completes");

    let state = &outcome.state_json;
    let decision = state
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision, not null");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("gave-up"),
        "the window must survive the refusal and finalize on its next round, \
         got {decision:?}"
    );

    // The refusal REALLY fired (this is what keeps a compile-error
    // corrective loop from impersonating this path — see
    // `logged_turn_texts`'s doc): the budget corrective reached the model,
    // and the spawning block itself compiled (no GHC corrective before the
    // first child's turn).
    let turns = logged_turn_texts(&log_path);
    assert!(
        turns.iter().any(|t| t.contains("Fork budget exhausted")),
        "the budget refusal corrective must reach the model; logged turns:\n{turns:#?}"
    );
    assert!(
        !turns.iter().any(|t| t.contains("A block did not compile")),
        "no turn may be a GHC corrective — the spawning block must have compiled; \
         logged turns:\n{turns:#?}"
    );
}

/// F1 regression (driver structural review, 2026-08-23): a SETTLED green
/// thread's realm must be closed at the round-boundary sweep. A settle parks
/// the thread's `AsyncDoneWith` frame forever by design (the arm never
/// resumes it), so an unswept settled realm is a permanently-parked hole on
/// the shared outer session — the machine is never quiescent again, and
/// rotation at the fragment ceiling refuses, killing a long run hours after
/// the causal block. Ceiling 1 forces cycle 2's maintenance to demand
/// quiescence; pre-fix this errored with "not quiescent (2 parked hole(s))".
/// Cycle 2 forks again AFTER the rotation, so this also pins that the green
/// scheduler works on a freshly rotated machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settled_threads_leave_the_machine_quiescent_for_rotation() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    // Process-isolated under nextest (one process per test), same idiom as
    // `machine_rotation_between_cycles_preserves_durable_state`.
    std::env::set_var("TIDEPOOL_MACHINE_FRAGMENT_CEILING", "1");

    let replies = vec![
        // Cycle 1: spawn two async forks, wait both, finalize.
        reply(ASYNC_FORK_BLOCK),
        finalize_int_reply(1),
        finalize_int_reply(2),
        // Cycle 2, post-rotation: the same composition again.
        reply(ASYNC_FORK_BLOCK),
        finalize_int_reply(3),
        finalize_int_reply(4),
    ];
    let (mut driver, _agent, _log_path) = build_driver(replies, "answerer-async-fork-quiescent");
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome1 = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("cycle 1: async forks settle, sweep closes their realms");
    let outcome2 = driver
        .run_one_loop_iteration(&source, Some(&outcome1.state_json))
        .await
        .expect(
            "cycle 2 must rotate cleanly at the forced ceiling — a settled \
             thread's realm left open makes the machine permanently \
             non-quiescent and this errors 'not quiescent (N parked hole(s))'",
        );

    let decision = outcome2
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("cycle 2's Decision landed");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("34"),
        "post-rotation forks still deliver to the right handles (3×10 + 4), \
         got {decision:?}"
    );
}

/// What the H2 gate probe records: every retirement and failure attribution
/// the fork children's `BranchAgentSessionGuard`/`drive_fork_child_agent_session` reports, plus
/// whether the driver ever mistakenly asked the operator anything (it must
/// not — nothing in this scenario suspends on `askUser`).
#[derive(Default)]
struct RetireProbe {
    present_form_calls: AtomicUsize,
    retired: Mutex<Vec<String>>,
    failed: Mutex<Vec<(String, String)>>,
}

struct ProbeGate {
    probe: Arc<RetireProbe>,
}

impl OperatorGate for ProbeGate {
    fn present_form(&self, _shape: &FormShape) -> serde_json::Value {
        self.probe.present_form_calls.fetch_add(1, Ordering::SeqCst);
        json!("unexpected — this scenario never asks the operator anything")
    }

    fn retire_node(&self, label: &str) {
        self.probe.retired.lock().unwrap().push(label.to_string());
    }

    fn node_failed(&self, label: &str, reason: &str) {
        self.probe
            .failed
            .lock()
            .unwrap()
            .push((label.to_string(), reason.to_string()));
    }
}

/// H2, revised for the fork-child-failure-semantics change (operator
/// decision, 2026-08-24): an ASYNC fork child (`async (fork @T brief)`,
/// NOT a `runLLMTurnFork`/`Fanout` BRANCH POSITION) that exhausts its own
/// round budget without ever finalizing no longer hard-fails the whole
/// cycle. Instead the block that was `wait`-ing on it (here, the SAME
/// spawning block that `wait`s both children) is ABORTED with a plain-
/// language corrective naming the child by its derived path — the parent
/// session and the run survive, and the parent finalizes plainly on its
/// next round. What this pins: (1) the corrective reaches the model and
/// names the starved child's path, with no internal identifier
/// (`InvocationExit`/`ExitRoundsExhausted`) leaking into model-facing
/// text; (2) the settled sibling "pick a" — which finalized successfully
/// BEFORE "pick b" starved — still retires and reports `node_finalized`,
/// proving its own fork completed cleanly and was not corrupted by "pick
/// b"'s failure, even though the aborted block never got to use its
/// value; (3) only "pick b" is reported `node_failed`, and that reason
/// (an operator-GUI/log channel, not model-facing) still carries the
/// round-exhaustion detail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_fork_child_round_exhaustion_aborts_block_with_corrective_and_run_survives() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let starved = || reply("Still weighing this branch; nothing to run yet.");
    let replies = vec![
        // 1. The same two-async-fork spawning block as the composition test.
        reply(ASYNC_FORK_BLOCK),
        // 2. Fork child A ("pick a") — answers normally in its one round.
        finalize_int_reply(1),
        // 3-5. Fork child B ("pick b") — three prose-only replies with no
        //    ```haskell block, burning its (lowered) round budget: cap 1 +
        //    2 ultimatum-grace rounds = 3 (`drive_agent_session_to_finalize`'s
        //    `hard_rounds = max_rounds + 2`).
        starved(),
        starved(),
        starved(),
        // 6. The recovery round after the corrective aborts the spawning
        //    block: finalize plainly, same shape the budget-refusal tests
        //    use for their own recovery round.
        reply(
            "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
             (finalize @Decision (Decision { action = \"gave-up\", rationale = \"fork \
             child failed\", confidence = Medium }) :: M ())\n```",
        ),
    ];
    let (mut driver, _agent, log_path) = build_driver(replies, "answerer-async-starved");
    // Lower the shared round cap so the starved child hits round exhaustion
    // in 3 scripted replies instead of 34 — this also caps the parent's and
    // child A's own budgets, but both finalize inside their first round
    // regardless.
    driver.set_answerer_round_caps(0, 1);
    let probe = Arc::new(RetireProbe::default());
    driver.set_gate(Arc::new(ProbeGate {
        probe: probe.clone(),
    }));
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver.run_one_loop_iteration(&source, None).await.expect(
        "a starved async fork child aborts the consuming block with a corrective — \
             the run must survive and finalize on the recovery round",
    );

    let decision = outcome
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision, not null");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("gave-up"),
        "the window must survive the starved child's failure and finalize on its next \
         round, got {decision:?}"
    );

    // No leaked Running node: BOTH fork children's own GUI entries retire
    // (unconditional in `drive_fork_child_agent_session`, before it branches on
    // outcome) — the starved one reported failed, the settled sibling not.
    assert_eq!(
        probe.retired.lock().unwrap().as_slice(),
        ["root/f0-pick-a".to_string(), "root/f1-pick-b".to_string()],
        "both fork children's derived labels must retire, in spawn order — a leaked \
         Running node would be a label never appearing here"
    );
    let failed = probe.failed.lock().unwrap();
    assert_eq!(
        failed.len(),
        1,
        "exactly ONE node_failed call — the settled sibling \"pick a\" must never be \
         reported failed: {failed:?}"
    );
    assert_eq!(
        failed[0].0, "root/f1-pick-b",
        "the node_failed call must name the starved child, not the settled sibling: \
         {failed:?}"
    );
    assert!(
        failed[0].1.contains("ExitRoundsExhausted"),
        "the operator-GUI node_failed reason (not model-facing) must still carry the \
         round-exhaustion detail: {}",
        failed[0].1
    );

    // The corrective really reached the MODEL — a distinctive needle naming
    // the starved child by its derived path — and no turn is a GHC
    // corrective loop impersonating this path (same discipline as
    // `logged_turn_texts`'s doc on this file's other tests).
    let turns = logged_turn_texts(&log_path);
    assert!(
        turns
            .iter()
            .any(|t| t.contains("root/f1-pick-b") && t.contains("exhausted its model rounds")),
        "the corrective must name the starved child by its path and say what happened; \
         logged turns:\n{turns:#?}"
    );
    assert!(
        !turns
            .iter()
            .any(|t| t.contains("InvocationExit") || t.contains("ExitRoundsExhausted")),
        "model-facing text must never carry an internal identifier \
         (docs/GLOSSARY.md prompt rules); logged turns:\n{turns:#?}"
    );
    assert!(
        !turns.iter().any(|t| t.contains("A block did not compile")),
        "no turn may be a GHC corrective; logged turns:\n{turns:#?}"
    );
    assert_eq!(
        probe.present_form_calls.load(Ordering::SeqCst),
        0,
        "nothing in this scenario suspends on askUser — the operator must never be asked"
    );
}

/// Rotation must not be blocked by a fork-child-failure block abort: with
/// the fragment ceiling forced to 1, cycle 1's spawning block aborts on a
/// starved fork child (same shape as the test above) and recovers, then
/// cycle 2 must still rotate cleanly and complete — pinning that the
/// abort's round-boundary sweep leaves the machine quiescent, the same
/// property `settled_threads_leave_the_machine_quiescent_for_rotation`
/// pins for a SUCCESSFUL settle (`sweep_green_round` closes every still-open
/// thread realm — Running or Settled — unconditionally, so the failure path
/// riding the SAME sweep is covered by construction; this is the receipt).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_child_failure_abort_still_leaves_machine_quiescent_for_rotation() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    std::env::set_var("TIDEPOOL_MACHINE_FRAGMENT_CEILING", "1");

    let starved = || reply("Still weighing this branch; nothing to run yet.");
    let replies = vec![
        // Cycle 1: two async forks, "pick a" answers, "pick b" starves and
        // aborts the block; the recovery round finalizes plainly.
        reply(ASYNC_FORK_BLOCK),
        finalize_int_reply(1),
        starved(),
        starved(),
        starved(),
        reply(
            "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
             (finalize @Decision (Decision { action = \"gave-up\", rationale = \"fork \
             child failed\", confidence = Medium }) :: M ())\n```",
        ),
        // Cycle 2, post-rotation: the ordinary composition succeeds cleanly.
        reply(ASYNC_FORK_BLOCK),
        finalize_int_reply(3),
        finalize_int_reply(4),
    ];
    let (mut driver, _agent, _log_path) =
        build_driver(replies, "answerer-async-fork-failure-quiescent");
    driver.set_answerer_round_caps(0, 1);
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome1 = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("cycle 1: the starved child's failure aborts the block, not the run");
    let decision1 = outcome1
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("cycle 1 must finalize on its recovery round");
    assert_eq!(
        decision1.get("action").and_then(|v| v.as_str()),
        Some("gave-up")
    );

    let outcome2 = driver
        .run_one_loop_iteration(&source, Some(&outcome1.state_json))
        .await
        .expect(
            "cycle 2 must rotate cleanly at the forced ceiling — a failed fork child's \
             realm left open would make the machine permanently non-quiescent and this \
             errors 'not quiescent (N parked hole(s))'",
        );
    let decision2 = outcome2
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("cycle 2's Decision landed");
    assert_eq!(
        decision2.get("action").and_then(|v| v.as_str()),
        Some("34"),
        "post-rotation forks still deliver to the right handles (3×10 + 4), got {decision2:?}"
    );
}

// ---------------------------------------------------------------------------
// fork-child-gui (folded, test-architecture review W1, 2026-08-23): a fork
// child gets the SAME operator-GUI/tree lifecycle a `runLLMTurnBranchLabeled`
// child gets (`tests/labeled_branch.rs`), even though nothing on the wire
// hands it a label — `SelfHarnessDriver::fork_child_label` derives one
// instead. Pins the three behaviors `labeled_branch.rs` pins for a
// wire-labeled branch child, against a fork child instead: its seed reaches
// the gate at birth, its own `askUser` routes to its OWN per-node gate
// (never the default one), and its finalize reaches `node_finalized` at its
// fold.
//
// The fixture's derived label is known exactly: the per-loop answerer (the
// fork's parent) has no companion `NodePath` and no registered GUI label of
// its own, so `fork_child_label` falls back to the fixed root id `"root"`;
// this is the first (and only) fork from that parent in the run, so
// `idx == 0`; the brief `"explore"` is already a bare lowercase word, so it
// slugs to itself — giving `"root/f0-explore"`.

fn fork_child_gui_header() -> LogHeader {
    LogHeader {
        prelude_hash: "fork-child-gui".into(),
        extract_fingerprint: "fork-child-gui".into(),
        harness_version: "test".into(),
    }
}

fn code(block: &str) -> RecordedReply {
    RecordedReply {
        content: format!("```haskell\n{block}\n```"),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

/// What the fork-child-gui test gates share — same shape as
/// `labeled_branch.rs`'s `RoutingProbe`.
#[derive(Default)]
struct RoutingProbe {
    default_present_calls: AtomicUsize,
    child_present_calls: AtomicUsize,
    retired: Mutex<Vec<String>>,
    seeded: Mutex<Vec<(String, String)>>,
    finalized: Mutex<Vec<(String, String)>>,
    failed: Mutex<Vec<(String, String)>>,
}

/// The driver's default gate: registers a per-node gate ONLY for the fork
/// child's DERIVED label (see this module's doc for the exact derivation) —
/// mirrors `labeled_branch.rs`'s `DefaultGate` shape, just against a label
/// this fixture computed by hand instead of one a caller chose.
struct DefaultGate {
    probe: Arc<RoutingProbe>,
    child_label: &'static str,
}

impl OperatorGate for DefaultGate {
    fn present_form(&self, _shape: &FormShape) -> serde_json::Value {
        self.probe
            .default_present_calls
            .fetch_add(1, Ordering::SeqCst);
        json!("unexpected — the fork child's own ask must never reach the default gate")
    }

    fn node_gate(&self, label: &str) -> Option<Arc<dyn OperatorGate>> {
        (label == self.child_label).then(|| {
            Arc::new(ChildGate {
                probe: self.probe.clone(),
            }) as Arc<dyn OperatorGate>
        })
    }

    fn retire_node(&self, label: &str) {
        self.probe.retired.lock().unwrap().push(label.to_string());
    }

    fn node_seeded(&self, label: &str, seed: &str) {
        self.probe
            .seeded
            .lock()
            .unwrap()
            .push((label.to_string(), seed.to_string()));
    }

    fn node_finalized(&self, label: &str, value: &str) {
        self.probe
            .finalized
            .lock()
            .unwrap()
            .push((label.to_string(), value.to_string()));
    }

    fn node_failed(&self, label: &str, reason: &str) {
        self.probe
            .failed
            .lock()
            .unwrap()
            .push((label.to_string(), reason.to_string()));
    }
}

/// The fork child's OWN gate — a distinct `Arc<dyn OperatorGate>` the driver
/// must resolve to via `node_gate` for every ask this node raises.
struct ChildGate {
    probe: Arc<RoutingProbe>,
}

impl OperatorGate for ChildGate {
    fn present_form(&self, _shape: &FormShape) -> serde_json::Value {
        self.probe
            .child_present_calls
            .fetch_add(1, Ordering::SeqCst);
        json!("a scripted answer")
    }
}

/// A fork child's `askUser` form reaches its OWN derived per-node gate
/// (never the default one), it seeds with the authored brief at birth, and
/// it finalizes and retires exactly once at its fold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_child_asks_route_to_its_own_derived_gate_and_finalizes() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // The per-loop answerer's own turn: fork ONE child, then finalize
        // with its answer — all one compiled block. The resume after fork
        // continues the SAME block via the session's own continuation, no
        // extra model round (same shape `acceptance_fork.rs`'s parent turn
        // uses for `forkAll` + `finalize`).
        code(
            "import Tidepool.Fork (fork)\n\n\
             do\n\
             \x20 n <- fork @Int \"explore\"\n\
             \x20 finalize @Int n :: M ()",
        ),
        // The fork child's own turn: one askUser round (a bare expression —
        // the WHOLE block — so resuming it completes the turn with no
        // further suspension). `askUser @T` is NULLARY (`Tidepool.Form.hs`).
        code("askUser @Text"),
        // The fork child's second turn: finalize with a typed Int.
        code("finalize @Int 99 :: M ()"),
    ];

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let log_path =
        std::env::temp_dir().join(format!("fork-child-gui-{}.jsonl", std::process::id()));
    let writer = tidepool_harness::log::LogWriter::create(&log_path, &fork_child_gui_header())
        .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let probe = Arc::new(RoutingProbe::default());
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_gate(Arc::new(DefaultGate {
        probe: probe.clone(),
        child_label: "root/f0-explore",
    }));

    let source = load_harness_source(&fixtures_dir().join("ForkChildGuiHarness.hs"))
        .expect("fork-child-gui fixture loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("render->loop->runLLMTurn->fork(1 child, asks+finalizes)->finalize cycle");

    assert_eq!(
        outcome.state_json.get("lastValue").and_then(|v| v.as_i64()),
        Some(99),
        "the parent must resume with the fork child's finalized Int and finalize \
         with it in turn: {:?}",
        outcome.state_json
    );

    assert_eq!(
        probe.child_present_calls.load(Ordering::SeqCst),
        1,
        "the fork child's one askUser form must reach its OWN derived per-node gate"
    );
    assert_eq!(
        probe.default_present_calls.load(Ordering::SeqCst),
        0,
        "the fork child's ask must never fall through to the default gate"
    );
    assert_eq!(
        probe.retired.lock().unwrap().as_slice(),
        ["root/f0-explore".to_string()],
        "the fork child's terminate/fold point must retire exactly its own \
         derived label, exactly once"
    );

    // The node-lifecycle extensions (seed at birth, outcome at fold): the
    // seed is the AUTHORED brief carried once at birth, not the composed
    // hole card, and this script finalizes, so the fold must attribute a
    // finalized VALUE, no failure.
    let seeded = probe.seeded.lock().unwrap();
    assert_eq!(seeded.len(), 1, "exactly one seed, at birth: {seeded:?}");
    assert_eq!(seeded[0].0, "root/f0-explore");
    assert_eq!(
        seeded[0].1, "explore",
        "the seed is the AUTHORED brief, not the composed hole card: {}",
        seeded[0].1
    );

    let finalized = probe.finalized.lock().unwrap();
    assert_eq!(
        finalized.len(),
        1,
        "one finalize at the fold: {finalized:?}"
    );
    assert_eq!(finalized[0].0, "root/f0-explore");
    assert_eq!(
        finalized[0].1, "99",
        "the finalized value is the fork child's own rendered answer: {}",
        finalized[0].1
    );
    assert!(
        probe.failed.lock().unwrap().is_empty(),
        "a fork child that finalizes has no failure to attribute"
    );
}
