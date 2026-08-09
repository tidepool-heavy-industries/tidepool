//! The outer driver lifecycle carries failure instead of publishing a
//! cosmetic `Idle`, and an errored cycle discards the failed cycle's
//! resident state (the per-loop answerer, its framing, this cycle's
//! compaction, the inference-call counter, and — critically — the outer
//! resident session, which may be parked mid-fragment on a hole) so the next
//! cycle re-bootstraps cleanly rather than running against stale state.
//!
//! Drives the real `SelfHarnessDriver` + `Harness` against the reference
//! harness module (mirrors `tests/selfharness_spine.rs`), with a scripted
//! `ModelProvider` that fails on demand instead of a hand-wired
//! mini-harness — the failure is induced through the real turn loop, not
//! injected past it. Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on
//! PATH — run inside `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage,
};
use tidepool_harness::{
    answerer_decls, load_harness_source, DriverError, Harness, HarnessSource, LogObserver,
    NodeState, SelfHarnessDriver, SelfHarnessState,
};

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
        prelude_hash: "selfharness-lifecycle".into(),
        extract_fingerprint: "selfharness-lifecycle".into(),
        harness_version: "test".into(),
    }
}

fn source() -> HarnessSource {
    load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads")
}

fn decision_block(action: &str, confidence: &str) -> String {
    format!(
        "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
         (finalize @Decision (Decision {{ action = \"{action}\", rationale = \"because\", \
         confidence = {confidence} }}) :: M ())\n```"
    )
}

/// A [`ModelProvider`] that fails the first `fail_first` calls with a
/// [`ProviderError`] and serves `reply` (a scripted `finalize` block) for
/// every call after — every real GHC compile still runs; only the model
/// call itself is under test control, so the test decides exactly which
/// round of the real turn loop fails.
struct FlakyProvider {
    calls: AtomicU32,
    fail_first: u32,
    reply: String,
}

impl ModelProvider for FlakyProvider {
    async fn complete(
        &self,
        _req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n <= self.fail_first {
            return Err(ProviderError::Api(format!("scripted failure #{n}")));
        }
        Ok(TurnResponse {
            text: self.reply.clone(),
            usage: Usage {
                input_tokens: 50,
                output_tokens: 10,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

fn driver_over(provider: FlakyProvider, log_tag: &str) -> SelfHarnessDriver {
    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let dyn_provider: Arc<dyn DynModelProvider> = Arc::new(provider);
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "selfharness-lifecycle-{log_tag}-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent =
        Arc::new(Harness::new(writer, agent_cfg, dyn_provider).expect("agent harness boots"));
    SelfHarnessDriver::new(agent, Arc::new(LogObserver))
}

/// A cycle whose answerer turn fails (the real turn loop, not a bypassed
/// one) leaves `lifecycle()` as `Failed`, not the old cosmetic `Idle` — and
/// the FOLLOWING cycle on the SAME driver succeeds. The failed cycle's outer
/// session (parked mid-fragment on the `runLLMTurn` hole) must have been
/// discarded and re-bootstrapped rather than reused: if it had not been
/// discarded, the second `run_one_cycle` would hit the resident session's
/// own "already suspended" guard instead of completing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errored_cycle_leaves_lifecycle_failed_and_next_cycle_recovers() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let mut driver = driver_over(
        FlakyProvider {
            calls: AtomicU32::new(0),
            fail_first: 1,
            reply: decision_block("observe", "Medium"),
        },
        "recover",
    );
    let harness_source = source();

    let cycle1 = driver.run_one_cycle(&harness_source, None).await;
    assert!(
        cycle1.is_err(),
        "the scripted first model call fails, so the cycle must error"
    );
    assert!(
        matches!(driver.lifecycle(), SelfHarnessState::Failed { .. }),
        "an errored cycle must publish Failed, not the cosmetic Idle, got {:?}",
        driver.lifecycle()
    );

    let cycle2 = driver
        .run_one_cycle(&harness_source, None)
        .await
        .expect("the cycle after a failure must re-bootstrap and succeed");
    assert!(
        matches!(driver.lifecycle(), SelfHarnessState::Idle),
        "a successful cycle must publish Idle, got {:?}",
        driver.lifecycle()
    );
    assert_eq!(
        cycle2.state_json.get("loopCount").and_then(|v| v.as_i64()),
        Some(1),
        "the recovered cycle must run loop from a fresh bootstrap, got {:?}",
        cycle2.state_json
    );
    assert_eq!(
        cycle2.state_json.get("mode").and_then(|v| v.as_str()),
        Some("Deciding"),
    );
}

/// A bootstrap failure on a driver that has never run a cycle before (not
/// recovering from a prior `Failed`) leaves `lifecycle()` as `Failed`, not
/// the driver's starting `Idle` — and is an ORDINARY error (`DriverError`
/// other than `Poisoned`), distinct from the recovery-failure escalation
/// path. A later cycle against a working extract binary then succeeds,
/// since `Failed` discarded nothing to rebuild but is itself recoverable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_driver_bootstrap_failure_is_failed_not_idle() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let mut driver = driver_over(
        FlakyProvider {
            calls: AtomicU32::new(0),
            fail_first: 0,
            reply: decision_block("observe", "Medium"),
        },
        "fresh-bootstrap-fail",
    );
    let harness_source = source();
    assert!(matches!(driver.lifecycle(), SelfHarnessState::Idle));

    let original_extract = std::env::var("TIDEPOOL_EXTRACT").ok();
    std::env::set_var(
        "TIDEPOOL_EXTRACT",
        "/nonexistent/tidepool-extract-bin-fresh-bootstrap-test",
    );
    let first = driver.run_one_cycle(&harness_source, None).await;
    match original_extract {
        Some(v) => std::env::set_var("TIDEPOOL_EXTRACT", v),
        None => std::env::remove_var("TIDEPOOL_EXTRACT"),
    }
    assert!(
        first.is_err(),
        "a first-ever bootstrap against a nonexistent extract binary must fail"
    );
    assert!(
        !matches!(first, Err(DriverError::Poisoned(_))),
        "a fresh driver's first bootstrap failure must be an ordinary error, not Poisoned, got {first:?}"
    );
    assert!(
        matches!(driver.lifecycle(), SelfHarnessState::Failed { .. }),
        "a fresh driver's failed bootstrap must publish Failed, not the starting Idle, got {:?}",
        driver.lifecycle()
    );

    let second = driver
        .run_one_cycle(&harness_source, None)
        .await
        .expect("a working extract binary lets the driver recover from the fresh Failed");
    assert!(matches!(driver.lifecycle(), SelfHarnessState::Idle));
    assert_eq!(
        second.state_json.get("loopCount").and_then(|v| v.as_i64()),
        Some(1),
    );
}

/// When recovery from a `Failed` cycle cannot itself rebuild a usable outer
/// session, the driver escalates to `Poisoned` — and every public entry
/// point (`run_one_cycle`, `run_loop`, `restore`) then refuses with
/// `DriverError::Poisoned` instead of attempting to run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poisoned_driver_refuses_entry_points() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let mut driver = driver_over(
        FlakyProvider {
            calls: AtomicU32::new(0),
            fail_first: 1,
            reply: decision_block("observe", "Medium"),
        },
        "poison",
    );
    let harness_source = source();

    driver
        .run_one_cycle(&harness_source, None)
        .await
        .expect_err("the scripted first model call fails, so the cycle must error");
    assert!(matches!(
        driver.lifecycle(),
        SelfHarnessState::Failed { .. }
    ));

    // Recovery re-bootstraps from scratch (the prior cycle discarded `outer`)
    // — point it at an extract binary that cannot possibly exist, so THIS
    // bootstrap attempt fails deterministically, without depending on GHC.
    let original_extract = std::env::var("TIDEPOOL_EXTRACT").ok();
    std::env::set_var(
        "TIDEPOOL_EXTRACT",
        "/nonexistent/tidepool-extract-bin-poisoned-test",
    );
    let recovery = driver.run_one_cycle(&harness_source, None).await;
    match original_extract {
        Some(v) => std::env::set_var("TIDEPOOL_EXTRACT", v),
        None => std::env::remove_var("TIDEPOOL_EXTRACT"),
    }
    assert!(
        recovery.is_err(),
        "a re-bootstrap against a nonexistent extract binary must fail"
    );
    assert!(
        matches!(driver.lifecycle(), SelfHarnessState::Poisoned { .. }),
        "a re-bootstrap failure while recovering from Failed must escalate to Poisoned, got {:?}",
        driver.lifecycle()
    );

    assert!(matches!(
        driver.run_one_cycle(&harness_source, None).await,
        Err(DriverError::Poisoned(_))
    ));
    assert!(matches!(
        driver.run_loop(&harness_source, true).await,
        Err(DriverError::Poisoned(_))
    ));
    assert!(matches!(
        driver.restore(&harness_source).await,
        Err(DriverError::Poisoned(_))
    ));
}

/// F1's ghost-node repro: `retire_answerer` (called at the end of every
/// `run_loop_fragment`, regardless of whether the cycle succeeds or fails —
/// see the `result = ...; self.retire_answerer(); result` shape) must
/// TERMINALIZE the per-loop answerer node it retires, not just drop the
/// harness's convenience `NodeConvo`/session — a forever-loop retires one
/// answerer per cycle, so a `terminate_node` that silently no-ops (or a bare
/// session drop with no tree transition) would leave one ghost
/// `Running`/`Suspended` node behind PER CYCLE, growing without bound.
///
/// Drives TWO cycles on one driver — the same fail-then-recover shape as
/// `errored_cycle_leaves_lifecycle_failed_and_next_cycle_recovers` above
/// (cheap: cycle 1's scripted provider failure still forces + retires an
/// answerer node before the failing model call, with no GHC compile for its
/// own turn; only cycle 2 runs a real compile+finalize) — and asserts every
/// node either cycle created is terminal (`Done` or `Cancelled`) once its
/// cycle completes. decision_block's scripted reply never forks, so each
/// cycle creates exactly one node (the loop's answerer).
///
/// Mutation: revert `SelfHarnessDriver::retire_answerer`'s body to
/// `self.agent.drop_session(node)`-equivalent (no tree terminalization) —
/// this test must go RED (a retired-but-not-terminalized answerer node stays
/// `Running`/`Suspended`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retired_answerer_nodes_are_terminal_across_cycles() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let provider = FlakyProvider {
        calls: AtomicU32::new(0),
        fail_first: 1,
        reply: decision_block("observe", "Medium"),
    };
    let dyn_provider: Arc<dyn DynModelProvider> = Arc::new(provider);
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "selfharness-lifecycle-ghost-node-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let harness = Arc::new(Harness::new(writer, agent_cfg, dyn_provider).expect("agent boots"));
    let mut driver = SelfHarnessDriver::new(harness.clone(), Arc::new(LogObserver));
    let harness_source = source();

    let mut seen_before: Vec<_> = harness.tree().node_ids_after(None, usize::MAX).0;
    let mut retired_nodes = Vec::new();

    // Cycle 1: the scripted first model call fails — but `retire_answerer`
    // still runs (it is called unconditionally after
    // `run_loop_fragment_inner`), so this already-forced answerer node must
    // still be retired terminally.
    let cycle1 = driver.run_one_cycle(&harness_source, None).await;
    assert!(
        cycle1.is_err(),
        "the scripted first model call must fail this cycle"
    );
    let (after_cycle1, _) = harness.tree().node_ids_after(None, usize::MAX);
    let new_in_cycle1: Vec<_> = after_cycle1
        .iter()
        .copied()
        .filter(|id| !seen_before.contains(id))
        .collect();
    assert_eq!(
        new_in_cycle1.len(),
        1,
        "cycle 1 must create exactly one node (the loop's answerer), got {new_in_cycle1:?}"
    );
    retired_nodes.extend(new_in_cycle1);
    seen_before = after_cycle1;

    // Cycle 2: the model call now succeeds — a real GHC-compiled turn +
    // finalize, same as `errored_cycle_leaves_lifecycle_failed_and_next_cycle_recovers`.
    driver
        .run_one_cycle(&harness_source, None)
        .await
        .expect("cycle 2 (after the recovered driver) must succeed");
    let (after_cycle2, _) = harness.tree().node_ids_after(None, usize::MAX);
    let new_in_cycle2: Vec<_> = after_cycle2
        .iter()
        .copied()
        .filter(|id| !seen_before.contains(id))
        .collect();
    assert_eq!(
        new_in_cycle2.len(),
        1,
        "cycle 2 must create exactly one node (the loop's answerer), got {new_in_cycle2:?}"
    );
    retired_nodes.extend(new_in_cycle2);

    for node in &retired_nodes {
        let state = harness.tree().state(*node);
        assert!(
            matches!(
                state,
                Some(NodeState::Done) | Some(NodeState::Cancelled { .. })
            ),
            "retired answerer node {node:?} must be terminal, got {state:?}"
        );
    }
}
