//! Acceptance for PRD 20 S1-L4 ("concurrent cognition windows"): the
//! AUTHORED outer loop's `runLLMTurnFanout` is now serviced by the driver
//! CONCURRENTLY — up to [`SelfHarnessDriver::set_concurrency_cap`] children
//! at once, each in its own freshly-minted answerer realm on the shared
//! outer machine — rather than one at a time
//! (`SelfHarnessDriver::service_outer_fanout`).
//!
//! ONE fixture (`fixtures/ConcurrentFanoutHarness.hs`, a fan of nine
//! prompts), ONE compile shape shared across both tests below
//! (family-bundle discipline: the compile memo makes the second test's
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
        prelude_hash: format!("outer-fanout-{label}"),
        extract_fingerprint: format!("outer-fanout-{label}"),
        harness_version: "test".into(),
    }
}

/// A [`ModelProvider`] that answers each fanout child by matching a fixed
/// NEEDLE against the request's last message (the hole card embeds the
/// fanout element's own prompt verbatim, e.g. `"FANOUT-3"` — a substring
/// distinguishing which child this call is), rather than by call ORDER —
/// `ReplayProvider`'s strict FIFO queue cannot serve concurrent children,
/// since which order their provider calls actually arrive in is exactly
/// the thing under test.
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
        let last = req
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let (needle, reply) = self
            .scripted
            .iter()
            .find(|(needle, _)| last.contains(needle))
            .cloned()
            .ok_or_else(|| {
                ProviderError::Api(format!(
                    "KeyedProvider: no scripted reply matches the request:\n{last}"
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

/// Drive one `run_one_cycle` against the fixture with the given provider and
/// concurrency cap, returning the resumed `answers` list.
async fn run_fanout_cycle(provider: KeyedProvider, concurrency_cap: usize) -> Vec<i64> {
    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
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

    let source = load_harness_source(&fixtures_dir().join("ConcurrentFanoutHarness.hs"))
        .expect("concurrent-fanout fixture loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one render->loop->runLLMTurnFanout->finalize->render cycle");

    outcome
        .state_json
        .get("answers")
        .and_then(|v| v.as_array())
        .expect("answers is a JSON array")
        .iter()
        .map(|v| v.as_i64().expect("each answer is an Int"))
        .collect()
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
        .run_one_cycle(&source, None)
        .await
        .expect("one render->loop->runLLMTurnFanout->finalize->render cycle");

    let answers: Vec<i64> = outcome
        .state_json
        .get("answers")
        .and_then(|v| v.as_array())
        .expect("answers is a JSON array")
        .iter()
        .map(|v| v.as_i64().expect("each answer is an Int"))
        .collect();
    assert_eq!(answers, (0..9).collect::<Vec<i64>>());

    assert_eq!(
        provider.peak_active(),
        8,
        "peak concurrently in-flight provider calls must be exactly the cap (8) — \
         not 9 (the cap must hold) and not fewer (the 8 must genuinely overlap, or this \
         assertion would pass by timing accident rather than by the cap)"
    );
}
