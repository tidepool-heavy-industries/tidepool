//! Acceptance coverage for state injection: two
//! consecutive `SelfHarnessDriver::run_one_loop_iteration` calls over the reference
//! generic-assistant harness (`examples/harness/Harness.hs`), each pushing
//! genuinely DIFFERENT `State` through the fused outer render/loop compile
//! (`SelfHarnessDriver::compile_loop_entry`).
//!
//! Two independent proofs:
//!
//! 1. **Rendered outer module byte-identical.** The `Event::OuterCompile{label:
//!    "render+loop", source}` this driver emits every cycle is captured for
//!    both cycles and asserted equal, despite the state/loopCount genuinely
//!    differing — the module TEXT no longer carries the state's own bytes.
//! 2. **The second cycle's outer compile is a memo HIT.** With a PINNED
//!    PRIVATE compile memo (`support::isolate_compile_memo`, never shared
//!    with another test process), the total `tidepool-extract` spawn count
//!    for cycle 2 is exactly ONE LESS than cycle 1's — the fused
//!    render+loop compile's own spawn (the only one memo-sensitive in this
//!    accounting; see the per-cycle spawn breakdown comment below) drops
//!    out, while the deliberately-uncacheable harness-ctx bind still pays
//!    its cost every cycle.
//!
//! **DEMOTED: the answerer's own turn is never reached.** The model provider ALWAYS
//! fails its `complete()` call, so `run_one_loop_iteration` still runs the
//! fused render/loop compile (`compile_loop_entry`, called BEFORE the model
//! is ever invoked — see that method's own doc) but the loop never gets a
//! reply to extract+compile as the answerer's own turn: this eliminates the
//! 2 SESSION-SCOPED answerer compiles the prior version of this test paid
//! (one per cycle, via a scripted `ReplayProvider` reply), while the 2 fused
//! (non-session-scoped) compiles this test actually measures stay exactly as
//! they were — hand-built `State` JSONs stand in for the `ReplayProvider`
//! replies that used to produce them. `compile_loop_entry` itself is
//! `pub(crate)` — not reachable directly from this crate's integration tests
//! without widening its visibility, a `tidepool-harness/src` change outside
//! this lane's boundary — so an always-failing provider is the in-bounds way
//! to reach the identical code path through the public
//! `run_one_loop_iteration` entry point, the same "standalone PROBE" spirit
//! `agent_stack_scoping.rs`/`finalize_type_pinning.rs`/
//! `dogfood_harness_typecheck.rs` already use (there, by calling a public
//! lower-level compile fn directly; here, by starving the public entry point
//! of the one thing that would make it pay for more than what's measured).
//! This drops the prior version's "the run still observes the SECOND state's
//! values" tertiary check (which needs a COMPLETED loop, i.e. a real answerer
//! reply) — that property (injected state genuinely flowing through, not
//! staying stale) is already proven, over this identical `compile_loop_entry`
//! mechanism, by `acceptance_selfharness.rs`'s
//! `selfharness_multi_cycle_state_accumulates_across_loop_boundaries`
//! (a real 2-cycle run whose whole point is that state accumulates and is
//! read back correctly).
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::json;

mod support;

use tidepool_harness::engine;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse,
};
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Event, Harness, Observer, SelfHarnessDriver,
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
        prelude_hash: "state-injection-memo-hit".into(),
        extract_fingerprint: "state-injection-memo-hit".into(),
        harness_version: "test".into(),
    }
}

/// A [`ModelProvider`] that never lets the loop finish: every `complete()`
/// call fails, so the answerer's own reply is never extracted/compiled. This
/// is what lets the test reach `compile_loop_entry` (called BEFORE the model
/// is invoked) at zero session-scoped compile cost — see the module doc.
struct AlwaysFailingProvider;

impl ModelProvider for AlwaysFailingProvider {
    async fn complete(
        &self,
        _req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        Err(ProviderError::Api(
            "scripted: this test never lets the model answer".into(),
        ))
    }
}

/// Captures every [`Event::OuterCompile`] this driver emits, in order.
#[derive(Default)]
struct OuterCompileCapture {
    events: Mutex<Vec<Event>>,
}

impl Observer for OuterCompileCapture {
    fn on_event(&self, event: &Event) {
        self.events.lock().push(event.clone());
    }
}

impl OuterCompileCapture {
    /// The `source` of every `OuterCompile{label: "render+loop", ..}` event
    /// captured so far, in order — the fused compile's rendered module text,
    /// once per `compile_loop_entry` call.
    fn render_loop_sources(&self) -> Vec<String> {
        self.events
            .lock()
            .iter()
            .filter_map(|e| match e {
                Event::OuterCompile { label, source } if label == "render+loop" => {
                    Some(source.clone())
                }
                _ => None,
            })
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_cycle_outer_compile_is_a_memo_hit_with_fresh_state() {
    support::require_extract();
    // A PINNED PRIVATE memo — never shared with another test process, so
    // every spawn/hit this test observes is caused by ITS OWN two cycles,
    // not by ambient state a concurrently-running test process wrote.
    let _memo_guard = support::isolate_compile_memo();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(AlwaysFailingProvider);
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "state-injection-memo-hit-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let capture = Arc::new(OuterCompileCapture::default());
    let mut driver = SelfHarnessDriver::new(agent, capture.clone());
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    // Two hand-built, genuinely different `State` JSONs (matching
    // `HarnessTypes.State`'s shape) stand in for what two `ReplayProvider`
    // replies used to produce — no model round-trip needed to get them.
    let state1 = json!({ "mode": "Observing", "notes": [], "lastDecision": null });
    let state2 = json!({
        "mode": "Deciding",
        "notes": ["first loop"],
        "lastDecision": {
            "action": "observe",
            "rationale": "first loop",
            "confidence": "Medium",
        },
    });

    // Own process (nextest gives every test binary its own).
    engine::reset_extract_spawn_count();

    let before_cycle1 = engine::extract_spawn_count();
    let cycle1 = driver.run_one_loop_iteration(&source, Some(&state1)).await;
    let spawns_cycle1 = engine::extract_spawn_count() - before_cycle1;
    assert!(
        cycle1.is_err(),
        "the scripted provider always fails, so the cycle must error — this \
         test only needs the fused compile to have run, never the loop to finish: {cycle1:?}"
    );

    let before_cycle2 = engine::extract_spawn_count();
    let cycle2 = driver.run_one_loop_iteration(&source, Some(&state2)).await;
    let spawns_cycle2 = engine::extract_spawn_count() - before_cycle2;
    assert!(
        cycle2.is_err(),
        "cycle 2 also never gets past the always-failing provider: {cycle2:?}"
    );

    // --- Proof 1: the fused outer module's rendered TEXT is turn-invariant.
    let sources = capture.render_loop_sources();
    assert_eq!(
        sources.len(),
        2,
        "each cycle must emit exactly one render+loop OuterCompile event, got {sources:?}; \
         cycle1={cycle1:?}; cycle2={cycle2:?}"
    );
    assert_eq!(
        sources[0], sources[1],
        "the fused render/loop module's SOURCE must be byte-identical across cycles \
         carrying different State — state now crosses as an injected Text value \
         (state_cross::state_in_via_ctx), never as a source literal"
    );

    // --- Proof 2: cycle 2 pays STRICTLY FEWER extract spawns than cycle 1 —
    // the fused compile's own spawn dropping out as a memo hit. With the
    // model always failing, the fused compile + the harness-ctx bind
    // (`refresh_harness_ctx`, which NEVER memo-hits by design — fresh literal
    // content every cycle) are this test's ONLY spawn sources per cycle, so
    // the delta is exactly the fused compile's own hit: cycle 1 pays 2 (ctx
    // bind + fused compile, cold MISS), cycle 2 pays 1 (ctx bind + fused
    // compile, HIT).
    assert!(
        spawns_cycle2 < spawns_cycle1,
        "cycle 2 must pay fewer extract spawns than cycle 1 (the fused render/loop \
         compile's own spawn dropping out as a memo hit) — cycle 1 paid {spawns_cycle1}, \
         cycle 2 paid {spawns_cycle2}"
    );
}
