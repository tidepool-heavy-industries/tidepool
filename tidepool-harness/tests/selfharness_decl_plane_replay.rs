//! DRIVER-LEVEL regression for the decl-plane replay bug the
//! stable-effects-core lane found and documented in `tidepool-harness/CLAUDE.md`
//! ("Living decl plane" section): a top-level declaration a model makes in
//! one answerer round was not actually being committed to the session's
//! decl plane when the round's block ENDED on a declaration (no bind/expr
//! after it) — `Harness::run_multi_item_block` rejected such a block with a
//! "not something that runs" compile error BEFORE ever calling
//! `session.define_scoped_in`, contradicting its own doc comment ("ending on
//! a bare declaration compiles and persists fine but advances nothing").
//! `tests/stable_effects_core_decl_plane.rs` proved the decl plane ITSELF
//! (`SessionLib::define`) works fine when driven directly — this file proves
//! the fix at the level the bug actually lived at: the driver's per-round
//! commit-then-nudge sequencing.
//!
//! Fixture: `examples/harness/decl-replay-spike/{Harness,HarnessTypes}.hs` —
//! a `loop` with THREE sequential `runLLMTurn @(State -> State)` windows on
//! the SAME per-loop answerer node (the driver reuses one node across a
//! cycle's holes — see `Harness::session_decl_context`'s doc), so declaring
//! in window 1 and referencing in window 3 crosses two hole boundaries, not
//! just one retry round.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

mod support;

use std::sync::Arc;

use parking_lot::Mutex;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Event, Harness, Observer, SelfHarnessDriver,
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

fn spike_harness_dir() -> std::path::PathBuf {
    repo_root().join("examples/harness/decl-replay-spike")
}

/// A DECL-ONLY, multi-item block — the exact shape that used to be rejected
/// by `run_multi_item_block` WITHOUT ever committing its declarations: a
/// `data` type followed by a function over it, ending on the function decl
/// (no trailing bind/expr). The driver treats this as a wasted, non-finalize
/// round and nudges for a real answer on the SAME hole.
fn decl_reply() -> RecordedReply {
    RecordedReply {
        content: "```haskell\n\
                  import HarnessTypes (State (..))\n\n\
                  data Pace = Steady | Bursty\n\n\
                  paceOf :: Pace -> Int\n\
                  paceOf Steady = 1\n\
                  paceOf Bursty = 2\n\n\
                  bumpBy :: Int -> State -> State\n\
                  bumpBy n st = st { counter = counter st + n }\n\
                  ```"
        .to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

/// A plain `State -> State` finalize, built from `expr` — does NOT reference
/// the decl-plane helper (used for windows that must resolve without it).
fn edit_reply(expr: &str) -> RecordedReply {
    RecordedReply {
        content: format!(
            "```haskell\nimport HarnessTypes (State (..))\n\n\
             (finalize @(State -> State) ({expr}) :: M ())\n```"
        ),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "decl-replay-spike".into(),
        extract_fingerprint: "decl-replay-spike".into(),
        harness_version: "test".into(),
    }
}

#[derive(Default)]
struct CapturingObserver {
    events: Mutex<Vec<Event>>,
}

impl Observer for CapturingObserver {
    fn on_event(&self, event: &Event) {
        self.events.lock().push(event.clone());
    }
}

/// SAME-CYCLE, cross-window persistence: window 1 declares `bumpBy` on the
/// shared decl plane (a decl-only block, nudged — the round the fix now
/// commits before erroring), window 1 then finalizes WITHOUT the helper,
/// window 2 finalizes an unrelated edit, and window 3 — a THIRD, separate
/// hole on the same node, in the SAME cycle — finalizes `bumpBy 9` and it
/// MUST resolve: the declaration from window 1 is visible two hole
/// boundaries later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decl_in_window_one_resolves_in_window_three_same_cycle() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(spike_harness_dir()),
    )
    .expect("answerer engine config");
    let replies = vec![
        decl_reply(),             // window 1, round 1: declares bumpBy (nudged)
        edit_reply("\\st -> st"), // window 1, round 2: resolves without the helper
        edit_reply("\\st -> st { history = history st <> [\"window2\"] }"), // window 2: unrelated edit
        edit_reply("bumpBy 9"), // window 3: USES the window-1 helper
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("decl-replay-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let observer = Arc::new(CapturingObserver::default());
    let mut driver = SelfHarnessDriver::new(agent, observer.clone());
    let source = load_harness_source(&spike_harness_dir().join("Harness.hs"))
        .expect("spike harness source loads");

    let outcome = match driver.run_one_loop_iteration(&source, None).await {
        Ok(o) => o,
        Err(e) => {
            let events: Vec<_> = observer
                .events
                .lock()
                .iter()
                .map(|ev| format!("{ev:?}"))
                .collect();
            panic!("cycle failed: {e}\n\nevents:\n{}", events.join("\n"));
        }
    };
    assert_eq!(
        outcome.state_json.get("counter").and_then(|v| v.as_i64()),
        Some(9),
        "window 3's `bumpBy 9` (declared in window 1) must have applied, got {:?}",
        outcome.state_json
    );
    assert_eq!(
        outcome
            .state_json
            .get("history")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(1),
        "only window 2's edit appends to history, got {:?}",
        outcome.state_json
    );
}

// Cross-cycle persistence (declare in cycle N, reference in cycle N+1,
// including across a forced machine ROTATION between them) is pinned by
// `tests/selfharness_fn_finalize_spike.rs`'s
// `living_helper_survives_loop_boundary_and_rotation` — intentionally NOT
// duplicated here: same mechanism, and that test already drives it through
// the SAME `SelfHarnessDriver::run_one_loop_iteration` entry point this file uses.
// Cross-cycle persistence IS the intended contract (see
// `SelfHarnessDriver::open_outer_plane`'s doc: "top-level declarations a
// model defines persist BY NAME across loops AND across machine
// rotations") — that test now passes with this same fix.
