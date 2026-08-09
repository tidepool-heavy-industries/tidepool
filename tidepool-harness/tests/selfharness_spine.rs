//! Acceptance coverage for the self-iterating harness driver spine
//! (`plans/self-iterating-harness/07-impl-orchestration.md`): ONE full
//! `render` -> `loop` -> `runLLMTurn @Decision` -> `finalize` -> `render`
//! cycle, driven through the production entry point
//! (`SelfHarnessDriver::run_one_cycle`), against the reference harness
//! module (`examples/harness/Harness.hs`). Needs `TIDEPOOL_EXTRACT` and the
//! with-packages GHC on PATH — run inside `nix develop` (see
//! `haskell/CLAUDE.md`).

use std::sync::Arc;

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
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
        prelude_hash: "acceptance-selfharness-spine".into(),
        extract_fingerprint: "acceptance-selfharness-spine".into(),
        harness_version: "test".into(),
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: content.to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
        },
    }
}

/// ONE render -> loop -> runLLMTurn @Decision -> finalize -> render cycle:
/// the nested Agent answers `loop`'s single `runLLMTurn @Decision` hole by
/// `finalize`-ing a `Decision` value (its `confidence` field a NESTED
/// `Confidence` sum, proving a whole author-defined ADT — not just a flat
/// type — crosses the outer/nested-Agent boundary); the outer `State`
/// (author-typed `mode`/`lastDecision`) must survive the loop boundary and
/// be visible to the NEXT `render` call, and the driver's own
/// loop-iteration count must advance alongside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selfharness_spine_one_cycle_render_loop_finalize_render() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    // The nested answerer: the SCOPED `answerer_decls()` stack (gui + finalize
    // only, NO runLLMTurn — effect-scoping), with `examples/harness` as its
    // project_lib so an answerer's `finalize @Decision (...)` can `import
    // Harness (Decision(..), Confidence(..))` from the reference harness module
    // directly.
    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let replies = vec![reply(
        "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
         (finalize @Decision (Decision { action = \"observe\", rationale = \"first loop\", \
         confidence = Medium }) :: M ())\n```",
    )];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("selfharness-spine-{}.jsonl", std::process::id())),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("one full render->loop->runLLMTurn->finalize->render cycle");

    // The PRE-loop render reflects `initialState`: Observing mode, no prior
    // decision.
    assert!(
        outcome.prompt_before.contains("observing"),
        "pre-loop render should show the initial Observing mode, got:\n{}",
        outcome.prompt_before
    );
    assert!(
        outcome.prompt_before.contains("No decision made yet"),
        "pre-loop render should show no decision yet, got:\n{}",
        outcome.prompt_before
    );

    // The typed `Decision` survived the loop boundary into the serialized
    // `State` — `loop`'s `nextMode`/`notes`/`lastDecision` fold, round-tripped
    // through `state_out`. The loop-iteration count is a runtime fact, not
    // part of `State` — asserted against the driver directly.
    let state = &outcome.state_json;
    assert_eq!(
        state.get("mode").and_then(|v| v.as_str()),
        Some("Deciding"),
        "loop must advance Observing -> Deciding, got {state:?}"
    );
    assert_eq!(
        driver.iteration(),
        1,
        "the driver's iteration count must increment across the loop boundary"
    );
    let decision = state
        .get("lastDecision")
        .and_then(|v| v.as_object())
        .expect("lastDecision must be a Just Decision, not null");
    assert_eq!(
        decision.get("action").and_then(|v| v.as_str()),
        Some("observe")
    );
    assert_eq!(
        decision.get("confidence").and_then(|v| v.as_str()),
        Some("Medium"),
        "the NESTED Confidence field must survive the crossing intact, got {decision:?}"
    );
    let notes = state
        .get("notes")
        .and_then(|v| v.as_array())
        .expect("notes must be an array");
    assert!(
        notes.iter().any(|n| n.as_str() == Some("observe")),
        "the decision's action must be folded into notes, got {notes:?}"
    );

    // The POST-loop render reflects the NEW state — it reaches the
    // next render, not just the persisted JSON.
    assert!(
        outcome.prompt_after.contains("deciding what to do next"),
        "post-loop render should show the advanced Deciding mode, got:\n{}",
        outcome.prompt_after
    );
    assert!(
        outcome.prompt_after.contains("Medium"),
        "post-loop render should show the new decision's confidence, got:\n{}",
        outcome.prompt_after
    );
}
