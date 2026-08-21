//! Acceptance for the BULK sibling verb, `runLLMTurnBranchFanout`, serviced
//! by `SelfHarnessDriver::service_outer_branch_fanout`. This is the
//! branch-shaped sibling of `outer_fanout.rs`'s acceptance for
//! `runLLMTurnFanout` — same three contracts, carried over to the verb
//! whose children fork off a SHARED parent `ContextRef` instead of an empty
//! root:
//!
//! - **Operator decision** (this lane): sibling branch windows are ALWAYS
//!   driven concurrently, transparently — scheduling is never a
//!   model-visible choice. Children are serviced CONCURRENTLY — up to
//!   `SelfHarnessDriver::set_concurrency_cap` at once, each in its own
//!   freshly-minted answerer realm — rather than one at a time, and
//!   completion order never reaches the observable result.
//! - **PRD 21 locked decision 6**: a child window's abnormal exit folds as
//!   DATA at its own branch position (`Left InvocationExit`) instead of
//!   aborting the outer turn and erasing its siblings' finished answers.
//!
//! ONE fixture (`fixtures/ConcurrentBranchFanoutHarness.hs`, a fan of nine
//! labeled children off one frozen `ContextRef`), ONE compile shape shared
//! across every test below (family-bundle discipline: the compile memo
//! makes each later test's compile free once the first has run).
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage,
};
use tidepool_harness::selfharness::operator::{ContinueSignal, FormShape, OperatorGate};
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver,
};

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn fixtures_dir() -> std::path::PathBuf {
    repo_root().join("tidepool-harness/tests/fixtures")
}

fn header(label: &str) -> LogHeader {
    LogHeader {
        prelude_hash: format!("branch-fanout-{label}"),
        extract_fingerprint: format!("branch-fanout-{label}"),
        harness_version: "test".into(),
    }
}

/// A [`ModelProvider`] that answers each branch-fanout child by matching a
/// fixed NEEDLE against the request's messages (the hole card embeds the
/// branch element's own prompt verbatim, e.g. `"BRANCH-3"`), rather than by
/// call ORDER — `ReplayProvider`'s strict FIFO queue cannot serve concurrent
/// children, since which order their provider calls actually arrive in is
/// exactly the thing under test. Adapted verbatim from `outer_fanout.rs`'s
/// `KeyedProvider` — see that file's own doc for why order-based matching
/// cannot work here.
struct KeyedProvider {
    scripted: Vec<(&'static str, String)>,
    delays: HashMap<&'static str, Duration>,
    active: AtomicU32,
    peak_active: AtomicU32,
}

impl KeyedProvider {
    fn new(scripted: Vec<(&'static str, String)>) -> Self {
        KeyedProvider {
            scripted,
            delays: HashMap::new(),
            active: AtomicU32::new(0),
            peak_active: AtomicU32::new(0),
        }
    }

    fn with_delay(mut self, needle: &'static str, delay: Duration) -> Self {
        self.delays.insert(needle, delay);
        self
    }

    fn peak_active(&self) -> u32 {
        self.peak_active.load(Ordering::SeqCst)
    }
}

impl ModelProvider for KeyedProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let transcript = req
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let (needle, reply) = self
            .scripted
            .iter()
            .find(|(needle, _)| transcript.contains(needle))
            .cloned()
            .ok_or_else(|| {
                ProviderError::Api(format!(
                    "KeyedProvider: no scripted reply matches the request:\n{transcript}"
                ))
            })?;

        let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_active.fetch_max(now_active, Ordering::SeqCst);

        if let Some(delay) = self.delays.get(needle) {
            tokio::time::sleep(*delay).await;
        }

        self.active.fetch_sub(1, Ordering::SeqCst);

        Ok(TurnResponse {
            text: reply,
            usage: Usage {
                input_tokens: 50,
                output_tokens: 10,
                cached_input_tokens: None,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

/// The nine `finalize @Int (<i>)`-shaped scripted replies the fixture's nine
/// branch-fanout children (`BRANCH-0`..`BRANCH-8`) need — shared by every
/// test below.
fn nine_finalize_replies() -> Vec<(&'static str, String)> {
    const NEEDLES: [&str; 9] = [
        "BRANCH-0", "BRANCH-1", "BRANCH-2", "BRANCH-3", "BRANCH-4", "BRANCH-5", "BRANCH-6",
        "BRANCH-7", "BRANCH-8",
    ];
    NEEDLES
        .iter()
        .enumerate()
        .map(|(i, needle)| {
            (
                *needle,
                format!("```haskell\nfinalize @Int ({i} :: Int)\n```"),
            )
        })
        .collect()
}

/// Pull the fixture's two per-branch projections out of a finished cycle's
/// state: `answers` (the answers that arrived, in branch order) and
/// `outcomes` (one entry per BRANCH POSITION — `ok:<n>` or `exit:<reason>`).
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

/// Drive one `run_one_cycle` against the fixture with the given provider and
/// concurrency cap, returning the resumed `answers` list.
async fn run_branch_fanout_cycle(provider: KeyedProvider, concurrency_cap: usize) -> Vec<i64> {
    run_branch_fanout_cycle_with(provider, concurrency_cap, |_| {})
        .await
        .0
}

/// [`run_branch_fanout_cycle`], with a hook to configure the driver before
/// the cycle runs, returning BOTH per-branch projections.
async fn run_branch_fanout_cycle_with(
    provider: KeyedProvider,
    concurrency_cap: usize,
    configure: impl FnOnce(&mut SelfHarnessDriver),
) -> (Vec<i64>, Vec<String>) {
    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn tidepool_harness::provider::DynModelProvider> = Arc::new(provider);
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "branch-fanout-{}-{}.jsonl",
            std::process::id(),
            uuid_ish()
        )),
        &header("cycle"),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_concurrency_cap(concurrency_cap);
    configure(&mut driver);

    let source = load_harness_source(&fixtures_dir().join("ConcurrentBranchFanoutHarness.hs"))
        .expect("concurrent-branch-fanout fixture loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one render->loop->runLLMTurnBranchFanout->finalize->render cycle");

    answers_and_outcomes(&outcome.state_json)
}

/// A cheap process-local disambiguator for the log filename (no real UUID
/// dependency needed — just enough to keep two `#[tokio::test]`s in this
/// file from colliding on the same path).
fn uuid_ish() -> u64 {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// The core contract, carried over from `outer_fanout.rs`'s own: TWO (of the
/// fixture's nine) branch-fanout children are serviced CONCURRENTLY, and
/// completing in either order yields an IDENTICAL final result —
/// declaration order, never completion order, decides the assembled
/// `[Int]`. Proven by forcing each order in turn (`BRANCH-0` deliberately
/// slower in one run, `BRANCH-1` in the other) and asserting both runs
/// resume with the exact same `answers`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn branch_fanout_children_serviced_concurrently_completion_order_insensitive() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let delay = Duration::from_millis(60);

    // Run A: BRANCH-0 finishes LAST.
    let provider_a = KeyedProvider::new(nine_finalize_replies()).with_delay("BRANCH-0", delay);
    let answers_a = run_branch_fanout_cycle(provider_a, 9).await;

    // Run B: BRANCH-1 finishes LAST instead — the opposite completion order
    // for these two children.
    let provider_b = KeyedProvider::new(nine_finalize_replies()).with_delay("BRANCH-1", delay);
    let answers_b = run_branch_fanout_cycle(provider_b, 9).await;

    let expected: Vec<i64> = (0..9).collect();
    assert_eq!(
        answers_a, expected,
        "declaration order must hold regardless of which child was slow"
    );
    assert_eq!(
        answers_b, expected,
        "the SAME declaration-order result must hold with the opposite child slow"
    );
    assert_eq!(
        answers_a, answers_b,
        "completion order must never reach the observable result"
    );
}

/// The concurrency cap is respected: with nine branch-fanout children and a
/// cap of eight, PEAK concurrently in-flight provider calls must be exactly
/// eight — never nine (the cap holds), and not fewer (the eight run
/// genuinely concurrently rather than serialized) — the ninth waits for a
/// slot to free before its own provider call ever starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn branch_fanout_respects_concurrency_cap() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    // Every child is uniformly slow enough that, if the cap were not
    // enforced, all nine would overlap — this is what makes "peak == 8, not
    // 9" a genuine assertion about the cap rather than a timing accident.
    let mut provider = KeyedProvider::new(nine_finalize_replies());
    for (needle, _) in nine_finalize_replies() {
        provider = provider.with_delay(needle, Duration::from_millis(40));
    }

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    // Keep the CONCRETE `Arc<KeyedProvider>` so `peak_active()` is readable
    // after the cycle — `Harness::new` only needs the erased
    // `Arc<dyn DynModelProvider>` view, coerced from a clone of the same Arc.
    let provider = Arc::new(provider);
    let dyn_provider: Arc<dyn tidepool_harness::provider::DynModelProvider> = provider.clone();
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("branch-fanout-cap-{}.jsonl", std::process::id())),
        &header("cap"),
    )
    .expect("log writer");
    let agent =
        Arc::new(Harness::new(writer, agent_cfg, dyn_provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_concurrency_cap(8);

    let source = load_harness_source(&fixtures_dir().join("ConcurrentBranchFanoutHarness.hs"))
        .expect("concurrent-branch-fanout fixture loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one render->loop->runLLMTurnBranchFanout->finalize->render cycle");

    let (answers, _outcomes) = answers_and_outcomes(&outcome.state_json);
    assert_eq!(answers, (0..9).collect::<Vec<i64>>());

    assert_eq!(
        provider.peak_active(),
        8,
        "peak concurrently in-flight provider calls must be exactly the cap (8) — \
         not 9 (the cap must hold) and not fewer (the 8 must genuinely overlap, or this \
         assertion would pass by timing accident rather than by the cap)"
    );
}

/// PRD 21 locked decision 6, the whole reason `runLLMTurnBranchFanout`
/// answers a list of `Either`: ONE child's window exhausts its rounds while
/// its eight siblings finalize normally. The outer turn must still
/// COMPLETE, every sibling's answer must arrive, and the failed child must
/// arrive as a typed `InvocationExit` AT ITS OWN BRANCH POSITION — not as an
/// error that fails the turn and erases the eight results already produced.
///
/// The starved child is `BRANCH-4`, the MIDDLE of the fan, so the assertion
/// covers position preservation on both sides of the hole.
///
/// Scripted with no compiles at all on the failing branch: `BRANCH-4`'s
/// reply carries no ```haskell block, so every one of its rounds is a
/// `NoBlock` re-prompt and the round budget is spent without the child ever
/// running anything. Round caps are lowered to 1/2 (hard stop at max+2 = 4
/// rounds) so that costs four instant provider calls rather than 34.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn branch_fanout_round_exhausted_child_folds_as_data_without_erasing_siblings() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let mut scripted = nine_finalize_replies();
    let starved = scripted
        .iter_mut()
        .find(|(needle, _)| *needle == "BRANCH-4")
        .expect("BRANCH-4 is one of the nine scripted needles");
    starved.1 = "Still weighing BRANCH-4; nothing to run yet.".to_string();

    let (answers, outcomes) =
        run_branch_fanout_cycle_with(KeyedProvider::new(scripted), 9, |driver| {
            driver.set_answerer_round_caps(1, 2);
        })
        .await;

    // Every sibling's answer arrived, in declaration order, with the failed
    // branch contributing nothing rather than displacing anyone.
    assert_eq!(
        answers,
        vec![0, 1, 2, 3, 5, 6, 7, 8],
        "the eight finalizing siblings must all arrive; only child 4 is missing"
    );

    // …and the failure is DATA at position 4, typed, with the other eight
    // positions untouched.
    assert_eq!(outcomes.len(), 9, "one outcome per branch position");
    assert_eq!(
        outcomes[4], "exit:round exhaustion: runLLMTurn answerer exceeded 4 rounds (cap 2 + ultimatum grace) without finalizing",
        "child 4's branch position must carry the typed round-exhaustion exit, \
         rendered by `renderInvocationExit` — the message comes from \
         `drive_answerer_to_finalize` itself (shared with the sequential \
         `runLLMTurnBranch` path, since a branch-fanout sibling now gets the \
         SAME full capability, not a separate finalize-only round loop)"
    );
    for (i, outcome) in outcomes.iter().enumerate() {
        if i == 4 {
            continue;
        }
        assert_eq!(
            outcome,
            &format!("ok:{i}"),
            "branch {i} must still carry its own answer at its own position"
        );
    }
}

/// What the gate-seam-parity test below records: every `node_seeded`/
/// `node_finalized`/`node_failed` call the CONCURRENT driving path made,
/// keyed by label — mirrors `labeled_branch.rs`'s `RoutingProbe` fields for
/// the same three calls, minus the per-node routing bookkeeping that test
/// needs and this one does not (every lifecycle call here goes through
/// `self.gate` directly, never `resolve_gate`).
#[derive(Default)]
struct FanoutLifecycleProbe {
    seeded: Mutex<Vec<(String, String)>>,
    finalized: Mutex<Vec<(String, String)>>,
    failed: Mutex<Vec<(String, String)>>,
}

struct FanoutLifecycleGate {
    probe: Arc<FanoutLifecycleProbe>,
}

impl OperatorGate for FanoutLifecycleGate {
    fn present_form(&self, _shape: &FormShape) -> serde_json::Value {
        serde_json::json!(null)
    }

    fn await_continue(&self) -> ContinueSignal {
        ContinueSignal::Continue
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

/// The gate seam's node-lifecycle extensions
/// (`node_seeded`/`node_finalized`/`node_failed`) must cross with the same
/// fidelity for the CONCURRENT `drive_branch_fanout_child` path that
/// `tests/labeled_branch.rs` already pins for the SEQUENTIAL
/// `service_outer_branch` path: every sibling seeded at birth with its own
/// authored prompt (not a composed hole card), a finalized value on the
/// success path, and a typed failure reason on the round-exhaustion path —
/// with no cross-talk between concurrently-driven siblings sharing one
/// `Mutex`-guarded probe.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn branch_fanout_children_cross_the_gate_seam_with_sequential_fidelity() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let mut scripted = nine_finalize_replies();
    let starved = scripted
        .iter_mut()
        .find(|(needle, _)| *needle == "BRANCH-4")
        .expect("BRANCH-4 is one of the nine scripted needles");
    starved.1 = "Still weighing BRANCH-4; nothing to run yet.".to_string();

    let probe = Arc::new(FanoutLifecycleProbe::default());
    let gate_probe = probe.clone();
    let (answers, outcomes) =
        run_branch_fanout_cycle_with(KeyedProvider::new(scripted), 9, move |driver| {
            driver.set_answerer_round_caps(1, 2);
            driver.set_gate(Arc::new(FanoutLifecycleGate { probe: gate_probe }));
        })
        .await;

    // Sanity: the same behavior the sibling test above already pins.
    assert_eq!(answers, vec![0, 1, 2, 3, 5, 6, 7, 8]);
    assert_eq!(outcomes.len(), 9);

    let labels: Vec<String> = (1..=9).map(|i| format!("root/{i}-child")).collect();

    {
        let seeded = probe.seeded.lock().unwrap();
        assert_eq!(
            seeded.len(),
            9,
            "every one of the nine concurrent children must be seeded at birth: {seeded:?}"
        );
        for (i, label) in labels.iter().enumerate() {
            let entry = seeded
                .iter()
                .find(|(l, _)| l == label)
                .unwrap_or_else(|| panic!("{label} must have been seeded: {seeded:?}"));
            assert_eq!(
                entry.1,
                format!("BRANCH-{i}"),
                "the seed crossing the gate is the AUTHORED per-child prompt, not a \
                 composed hole card"
            );
        }
    }

    {
        let finalized = probe.finalized.lock().unwrap();
        assert_eq!(
            finalized.len(),
            8,
            "eight of the nine children finalize; only the starved BRANCH-4 sibling \
             (root/5-child) does not: {finalized:?}"
        );
        assert!(
            finalized.iter().all(|(l, _)| l != "root/5-child"),
            "the starved child must never be attributed a finalized value: {finalized:?}"
        );
    }

    {
        let failed = probe.failed.lock().unwrap();
        assert_eq!(
            failed.len(),
            1,
            "exactly one child fails — the starved one: {failed:?}"
        );
        assert_eq!(failed[0].0, "root/5-child");
        assert!(
            failed[0].1.contains("ExitRoundsExhausted"),
            "the failure reason is the InvocationExit rendering: {}",
            failed[0].1
        );
    }
}
