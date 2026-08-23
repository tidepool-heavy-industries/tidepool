//! Acceptance for the AUTHORED outer loop's `runLLMTurnFanout`, serviced by
//! `SelfHarnessDriver::service_outer_fanout`. Two contracts share one
//! fixture:
//!
//! - **PRD 20 S1-L4** ("concurrent cognition windows"): children are driven
//!   CONCURRENTLY — up to [`SelfHarnessDriver::set_concurrency_cap`] at once,
//!   each in its own freshly-minted answerer realm on the shared outer
//!   machine — rather than one at a time, and completion order never reaches
//!   the observable result.
//! - **PRD 21 locked decision 6**: a child window's abnormal exit folds as
//!   DATA at its own branch position (`Left InvocationExit`) instead of
//!   aborting the outer turn and erasing its siblings' finished answers.
//!
//! ONE fixture (`fixtures/ConcurrentFanoutHarness.hs`, a fan of nine
//! prompts), ONE compile shape shared across every test below
//! (family-bundle discipline: the compile memo makes each later test's
//! compile free once the first has run).
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage,
};
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Harness, LogObserver, SelfHarnessDriver,
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
        prelude_hash: format!("outer-fanout-{label}"),
        extract_fingerprint: format!("outer-fanout-{label}"),
        harness_version: "test".into(),
    }
}

/// A [`ModelProvider`] that answers each fanout child by matching a fixed
/// NEEDLE against the request's messages (the hole card embeds the fanout
/// element's own prompt verbatim, e.g. `"FANOUT-3"` — a substring
/// distinguishing which child this call is), rather than by call ORDER —
/// `ReplayProvider`'s strict FIFO queue cannot serve concurrent children,
/// since which order their provider calls actually arrive in is exactly
/// the thing under test.
///
/// The needle is looked for across the WHOLE transcript, not just the last
/// message: the driver's round loop pushes its own nudge and ultimatum turns
/// ("finalize now", "ROUND CAP REACHED") when a child is running out of
/// rounds, and those carry no needle. Each child is a fresh node with its own
/// transcript containing exactly one needle, so a whole-transcript scan is
/// still unambiguous.
///
/// Tracks concurrently in-flight calls (`active`/`peak_active`, PEAK
/// concurrency observed across the whole run) and supports an optional
/// per-needle artificial delay so a test can force a KNOWN completion order
/// deterministically.
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
                cache_write_tokens: None,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

/// The nine `finalize @Int (<i>) `-shaped scripted replies the fixture's
/// nine fanout prompts (`FANOUT-0`..`FANOUT-8`) need — shared by both tests
/// below.
fn nine_finalize_replies() -> Vec<(&'static str, String)> {
    const NEEDLES: [&str; 9] = [
        "FANOUT-0", "FANOUT-1", "FANOUT-2", "FANOUT-3", "FANOUT-4", "FANOUT-5", "FANOUT-6",
        "FANOUT-7", "FANOUT-8",
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

/// Drive one `run_one_loop_iteration` against the fixture with the given provider and
/// concurrency cap, returning the resumed `answers` list.
async fn run_fanout_cycle(provider: KeyedProvider, concurrency_cap: usize) -> Vec<i64> {
    run_fanout_cycle_with(provider, concurrency_cap, |_| {})
        .await
        .0
}

/// [`run_fanout_cycle`], with a hook to configure the driver before the cycle
/// runs, returning BOTH per-branch projections.
async fn run_fanout_cycle_with(
    provider: KeyedProvider,
    concurrency_cap: usize,
    configure: impl FnOnce(&mut SelfHarnessDriver),
) -> (Vec<i64>, Vec<String>) {
    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn tidepool_harness::provider::DynModelProvider> = Arc::new(provider);
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "outer-fanout-{}-{}.jsonl",
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

    let source = load_harness_source(&fixtures_dir().join("ConcurrentFanoutHarness.hs"))
        .expect("concurrent-fanout fixture loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("one render->loop->runLLMTurnFanout->finalize->render cycle");

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

/// PRD 20 S1-L4's core contract: TWO (of the fixture's nine) fanout
/// children are serviced CONCURRENTLY, and completing in either order
/// yields an IDENTICAL final result — declaration order, never completion
/// order, decides the assembled `[Int]`. Proven by forcing each order in
/// turn (`FANOUT-0` deliberately slower in one run, `FANOUT-1` in the
/// other) and asserting both runs resume with the exact same `answers`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outer_fanout_children_serviced_concurrently_completion_order_insensitive() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let delay = Duration::from_millis(60);

    // Run A: FANOUT-0 finishes LAST.
    let provider_a = KeyedProvider::new(nine_finalize_replies()).with_delay("FANOUT-0", delay);
    let answers_a = run_fanout_cycle(provider_a, 9).await;

    // Run B: FANOUT-1 finishes LAST instead — the opposite completion order
    // for these two children.
    let provider_b = KeyedProvider::new(nine_finalize_replies()).with_delay("FANOUT-1", delay);
    let answers_b = run_fanout_cycle(provider_b, 9).await;

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

/// The concurrency cap is respected: with nine fanout children and a cap of
/// eight, PEAK concurrently in-flight provider calls must be exactly eight
/// — never nine (the cap holds), and not fewer (the eight run genuinely
/// concurrently rather than serialized) — the ninth waits for a slot to
/// free before its own provider call ever starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outer_fanout_respects_concurrency_cap() {
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
        typed_request_agent_decls(),
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
        std::env::temp_dir().join(format!("outer-fanout-cap-{}.jsonl", std::process::id())),
        &header("cap"),
    )
    .expect("log writer");
    let agent =
        Arc::new(Harness::new(writer, agent_cfg, dyn_provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_concurrency_cap(8);

    let source = load_harness_source(&fixtures_dir().join("ConcurrentFanoutHarness.hs"))
        .expect("concurrent-fanout fixture loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("one render->loop->runLLMTurnFanout->finalize->render cycle");

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

/// PRD 21 locked decision 6, the whole reason `runLLMTurnFork`/
/// `runLLMTurnFanout` answer an `Either`: ONE child's window exhausts its
/// rounds while its eight siblings finalize normally. The outer turn must
/// still COMPLETE, every sibling's answer must arrive, and the failed child
/// must arrive as a typed `InvocationExit` AT ITS OWN BRANCH POSITION —
/// not as an error that fails the turn and erases the eight results already
/// produced (which is exactly what `service_outer_fanout` did while it
/// propagated a child's failure with `?`).
///
/// The starved child is `FANOUT-4`, the MIDDLE of the fan, so the assertion
/// covers position preservation on both sides of the hole.
///
/// Scripted with no compiles at all on the failing branch: `FANOUT-4`'s reply
/// carries no ```haskell block, so every one of its rounds is a `NoBlock`
/// re-prompt and the round budget is spent without the child ever running
/// anything. Round caps are lowered to 1/2 (hard stop at max+2 = 4 rounds) so
/// that costs four instant provider calls rather than 34.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outer_fanout_round_exhausted_child_folds_as_data_without_erasing_siblings() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let mut scripted = nine_finalize_replies();
    let starved = scripted
        .iter_mut()
        .find(|(needle, _)| *needle == "FANOUT-4")
        .expect("FANOUT-4 is one of the nine scripted needles");
    starved.1 = "Still weighing FANOUT-4; nothing to run yet.".to_string();

    let (answers, outcomes) = run_fanout_cycle_with(KeyedProvider::new(scripted), 9, |driver| {
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
    // Needled rather than a full-string `assert_eq!` (test-architecture
    // review J1, 2026-08-23): the old exact match coupled three volatile
    // things at once — `renderInvocationExit`'s prose, the cap arithmetic,
    // and the phrase "ultimatum grace" — so a wording pass could break this
    // pin without changing what it actually discriminates. These three
    // needles keep exactly that discriminating power (round exhaustion,
    // specifically child 4's, specifically after 4 rounds) while freeing the
    // prose to reword.
    assert!(
        outcomes[4].starts_with("exit:round exhaustion"),
        "child 4's branch position must carry the typed round-exhaustion exit, \
         rendered by `renderInvocationExit`, got: {}",
        outcomes[4]
    );
    assert!(
        outcomes[4].contains("child 4"),
        "the exit must name the failing child by position, got: {}",
        outcomes[4]
    );
    assert!(
        outcomes[4].contains("4 rounds"),
        "the exit must carry the hard-stop round count (cap 2 + 2 ultimatum grace), \
         got: {}",
        outcomes[4]
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
