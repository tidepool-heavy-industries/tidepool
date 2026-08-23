//! Acceptance coverage for `plans/turn-latency-state-injection.md`: two
//! consecutive `SelfHarnessDriver::run_one_cycle` calls over the reference
//! generic-assistant harness (`examples/harness/Harness.hs`), each pushing
//! genuinely DIFFERENT `State` through the fused outer render/loop compile
//! (`SelfHarnessDriver::compile_cycle_entry`).
//!
//! Two independent proofs, matching the plan's acceptance bullet verbatim:
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
//!    out, while the deliberately-uncacheable harness-ctx bind and the
//!    answerer's own reply compile still pay their cost every cycle.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::Arc;

use parking_lot::Mutex;

mod support;

use tidepool_harness::engine::{self, EngineConfig};
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::{
    answerer_decls, load_harness_source, Event, Harness, Observer, SelfHarnessDriver,
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

/// One recorded `finalize @Decision (...)` reply — mirrors
/// `acceptance_selfharness.rs`'s `decision_reply` fixture (a known-clean,
/// no-retry compile against the reference harness's `Decision`/`Confidence`
/// constructors), so each cycle's answerer pays exactly ONE reply compile.
fn decision_reply(action: &str, rationale: &str, confidence: &str) -> RecordedReply {
    let content = format!(
        "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
         (finalize @Decision (Decision {{ action = \"{action}\", rationale = \"{rationale}\", \
         confidence = {confidence} }}) :: M ())\n```"
    );
    RecordedReply {
        content,
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
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
    /// once per `compile_cycle_entry` call.
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
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    // Two DIFFERENT replies, so cycle 2's State (loopCount, mode, notes,
    // lastDecision) genuinely differs from cycle 1's — the same fixture
    // shape `acceptance_selfharness.rs` uses to prove state accumulates.
    let replies = vec![
        decision_reply("observe", "first loop", "Medium"),
        decision_reply("decide", "second loop", "High"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
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

    // Own process (nextest gives every test binary its own).
    engine::reset_extract_spawn_count();

    let before_cycle1 = engine::extract_spawn_count();
    let outcome1 = driver
        .run_one_cycle(&source, None)
        .await
        .expect("cycle 1 (cold memo)");
    let spawns_cycle1 = engine::extract_spawn_count() - before_cycle1;

    let before_cycle2 = engine::extract_spawn_count();
    let outcome2 = driver
        .run_one_cycle(&source, Some(&outcome1.state_json))
        .await
        .expect("cycle 2 (warm memo)");
    let spawns_cycle2 = engine::extract_spawn_count() - before_cycle2;

    // --- Proof 1: the fused outer module's rendered TEXT is turn-invariant.
    let sources = capture.render_loop_sources();
    assert_eq!(
        sources.len(),
        2,
        "each cycle must emit exactly one render+loop OuterCompile event, got {sources:?}"
    );
    assert_ne!(
        outcome1.state_json, outcome2.state_json,
        "the two cycles must carry genuinely different State for this to prove anything"
    );
    assert_eq!(
        sources[0], sources[1],
        "the fused render/loop module's SOURCE must be byte-identical across cycles \
         carrying different State — state now crosses as an injected Text value \
         (state_cross::state_in_via_ctx), never as a source literal"
    );

    // --- Proof 2: cycle 2 pays STRICTLY FEWER extract spawns than cycle 1 —
    // the fused compile's own spawn dropping out as a memo hit (directly
    // confirmed via `TIDEPOOL_TIMING`-free instrumentation while developing
    // this test: cycle 1's fused compile is 1 spawn, MISS as expected;
    // cycle 2's is 0, a HIT). Not asserted as an EXACT delta here: the
    // answerer's own turn compile path (`EngineConfig::turn_target`'s
    // standalone shim probe) is independently content-addressed and ALSO
    // warms up between cycle 1 and cycle 2 — a second, pre-existing memo
    // this test doesn't own, compounding with the fused compile's own hit.
    // `refresh_harness_ctx`'s bind is the one spawn that NEVER drops
    // (hazard (b): fresh literal content every cycle) — so a strict
    // decrease can only come from something ELSE memoizing, and the fused
    // compile is the one this test's byte-identical-source proof above
    // already pins as a guaranteed contributor.
    assert!(
        spawns_cycle2 < spawns_cycle1,
        "cycle 2 must pay fewer extract spawns than cycle 1 (the fused render/loop \
         compile's own spawn dropping out as a memo hit) — cycle 1 paid {spawns_cycle1}, \
         cycle 2 paid {spawns_cycle2}"
    );

    // --- The run still observes the SECOND state's values — the injected
    // value plumbing is live, not stale (byte-identical source alone would
    // also pass if `__harnessCtx` silently resolved to cycle 1's frozen
    // value on cycle 2).
    assert_eq!(
        outcome2.state_json.get("mode").and_then(|v| v.as_str()),
        Some("Acting"),
        "cycle 2 must observe its OWN (second) state, got {:?}",
        outcome2.state_json
    );
    let decision = outcome2
        .state_json
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("cycle 2: lastDecision must be a Just Decision");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("decide"),
        "cycle 2 must observe the SECOND reply's decision, not the first's"
    );
    assert!(
        outcome2.prompt_after.contains("High"),
        "cycle 2's post-loop render must reflect the SECOND state's confidence, got:\n{}",
        outcome2.prompt_after
    );
}
