//! Pins the LAZY boot contract end to end, through the REAL production path
//! (`Harness::new` -> `force` -> `run_to_hole_or_done`): `force` registers a
//! machine-less session, and the machine comes up on the node's first REAL
//! turn (`ResidentSession::run`, reached via `Harness::run_to_hole_or_done`).
//! Observed through `Harness::heap_stats`: `None` when the node has no live
//! session machine (never forced, terminal, or forced but not yet
//! bootstrapped), `Some` once a machine is live.
//!
//! GHC-heavy tier: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).
//!
//! # Do not delete `outer_session_boots_from_pure_render_then_loop_suspends_on_a_real_hole` as redundant
//!
//! `render`'s own reachable Core only directly builds `Val` (via `pure`); the
//! other four freer scaffolding constructors (`E`/`Union`/`Leaf`/`Node`) reach
//! its compiled table only via the transitive DataCon-closure walk over the
//! `Eff`-typed binder (`collectTransitiveDCons`,
//! `haskell/src/Tidepool/Translate.hs`). If that walk ever regresses to miss
//! one of the five, the outer session's lazy bootstrap off `render`'s pure
//! fragment would silently fail to carry the constructor the SUBSEQUENT real
//! `runLLMTurn` fragment needs on the same machine — this test is what would
//! catch that, because it lives in `tidepool-harness/tests/acceptance_*.rs`,
//! which is a mandatory gate for changes to that closure computation.

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver, SelfHarnessState,
    TurnOutcome,
};

mod support;

fn prelude_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "lazy-boot".into(),
        extract_fingerprint: "lazy-boot".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 100,
        output_tokens: 20,
        cached_input_tokens: None,
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: content.to_string(),
        usage: usage(),
    }
}

/// Pins the lazy-boot contract above for a plain node.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_machine_after_force_a_machine_after_the_first_turn() {
    if !support::extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("lazy_boot.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![reply(
        "Here's the answer.\n\n```haskell\npure (toJSON (21 * 2 :: Int))\n```",
    )];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    // `Harness::new` itself needs no GHC compile and would succeed without
    // `TIDEPOOL_EXTRACT`; the guard above covers what the rest of this test needs.
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("lazy-boot root", "What is 21 * 2?")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    assert_eq!(
        harness.heap_stats(root),
        None,
        "a freshly-forced node must have NO live machine yet — force() no longer \
         bootstraps eagerly"
    );

    let turn = harness
        .run_to_hole_or_done(root)
        .await
        .expect("the first real turn drives to completion");
    assert!(
        matches!(turn, TurnOutcome::Completed { .. }),
        "expected the turn to complete"
    );

    assert!(
        harness.heap_stats(root).is_some(),
        "the node's first real turn must have brought the machine up — \
         heap_stats should now report Some"
    );
}

fn examples_harness_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .join("examples/harness")
}

fn decision_block(action: &str, confidence: &str) -> String {
    format!(
        "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
         (finalize @Decision (Decision {{ action = \"{action}\", rationale = \"because\", \
         confidence = {confidence} }}) :: M ())\n```"
    )
}

/// A [`tidepool_harness::provider::ModelProvider`] that always succeeds with
/// a scripted `finalize` reply.
struct AlwaysReply(String);

impl tidepool_harness::provider::ModelProvider for AlwaysReply {
    async fn complete(
        &self,
        _req: tidepool_harness::provider::TurnRequest,
        _sink: Option<tidepool_harness::provider::StreamSink>,
    ) -> Result<tidepool_harness::provider::TurnResponse, tidepool_harness::provider::ProviderError>
    {
        Ok(tidepool_harness::provider::TurnResponse {
            text: self.0.clone(),
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

/// A FRESH `SelfHarnessDriver`'s outer session bootstraps lazily off
/// `render_framing`'s pure compile (its first REAL fragment). A single
/// ordinary cycle then drives `loop` (a REAL `runLLMTurn` call) on that SAME
/// machine to a suspend, services the hole through a real nested Agent turn,
/// and resumes to `finalize` — see the module doc for why this pins the
/// ConTags-reachability invariant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_session_boots_from_pure_render_then_loop_suspends_on_a_real_hole() {
    if !support::extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> =
        Arc::new(AlwaysReply(decision_block("observe", "Medium")));
    let writer = LogWriter::create(
        std::env::temp_dir().join(format!(
            "acceptance-lazy-boot-outer-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let harness = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(harness, Arc::new(LogObserver));
    let harness_source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("harness source loads");

    let cycle = driver.run_one_cycle(&harness_source, None).await.expect(
        "the outer session must bootstrap off render's pure fragment, then run loop \
             (a real runLLMTurn site) to a suspend/resume/finalize on the SAME machine",
    );
    assert!(
        matches!(driver.lifecycle(), SelfHarnessState::Idle),
        "an ordinary first cycle must publish Idle, got {:?}",
        driver.lifecycle()
    );
    // The loop-iteration count lives in the checkpoint ENVELOPE
    // (`driver.iteration()`), never in the authored `State`.
    assert_eq!(
        driver.iteration(),
        1,
        "the first-ever cycle must run loop exactly once, got {:?}",
        cycle.state_json
    );
}
