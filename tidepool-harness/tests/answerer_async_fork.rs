//! Acceptance coverage for the ANSWERER-PLANE green scheduler
//! (`SelfHarnessDriver::service_answerer_green`): the composed idiom
//! `async (fork @T brief)` — a model window spawning several fork children
//! as green threads, `wait`-ing their typed results, and finalizing from
//! them — plus the per-window fork budget's loud refusal path. Driven
//! through the production entry point (`SelfHarnessDriver::run_one_cycle`)
//! against the reference harness (`examples/harness/Harness.hs`), scripted
//! record-replay, zero live calls — the same discipline as
//! `selfharness_spine.rs`.
//!
//! The ReplayProvider queue is itself a behavior pin in both tests: the
//! composition test's queue only works if BOTH children are driven (in
//! spawn order) after the one spawning block; the budget test's queue has
//! NO second-child reply, so a refusal that failed to refuse would consume
//! the recovery finalize as the second child's turn and fail loudly.

use std::sync::Arc;

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver,
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
        answerer_decls(),
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
        .run_one_cycle(&source, None)
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
        .run_one_cycle(&source, None)
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
        .run_one_cycle(&source, None)
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
