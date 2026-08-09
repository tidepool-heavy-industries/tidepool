//! Extract-wave `boot` lane, item 0 steps 1-3 (+6): pins the LAZY boot
//! contract end to end, through the REAL production path
//! (`Harness::new` -> `force` -> `run_to_hole_or_done`).
//!
//! Before this item, `Harness::new` paid a GHC extract compile of a FAKE
//! program (`pure (toJSON (0 :: Int))`) up front to seed a boot-shaped
//! `ResidentSession::bootstrap`, and `force` bootstrapped every node's
//! session from that seed before any real turn ran. `ResidentSession`
//! construction is now lazy (`unbootstrapped`): `force` registers a
//! machine-less session, and the machine comes up on the node's first REAL
//! turn (`ResidentSession::run`, reached via `Harness::run_to_hole_or_done`).
//!
//! This test proves step 6 falls out as claimed: no machine right after
//! `force`, a live machine right after the first turn completes. It observes
//! this through `Harness::heap_stats`, which is `None` when the node has no
//! live session machine (never forced, terminal, OR — the new case this item
//! adds — forced but not yet bootstrapped) and `Some` once a machine is live.
//!
//! GHC-heavy tier: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).
//!
//! # The ConTags leg — what this file actually pins
//!
//! An earlier draft of this item's spec worried that the OUTER
//! (self-iterating-harness) session's lazy bootstrap might boot from a
//! ConTags-free expr, since its first real compile is the pure `render`
//! (`SelfHarnessDriver::render_framing`) rather than an effectful seed. That
//! concern does not hold: `ConTags::try_from` (`tidepool-codegen/src/effect_machine.rs`
//! 203-250) resolves the freer-simple SCAFFOLDING constructors (`Val`/`E`/
//! `Union`/`Leaf`/`Node`), not per-effect GADT constructors like
//! `RunLLMTurnWith` — and any `Eff`-typed term carries them, `pure` included
//! (`pure` at `Eff` literally builds a `Val`). The seed this item deletes was
//! ITSELF a pure `pure (toJSON (0 :: Int))` and that always resolved fine;
//! `render` cannot be worse-conditioned than the seed it replaces.
//!
//! So `outer_session_boots_from_pure_render_then_loop_suspends_on_a_real_hole`
//! below pins the REAL invariant instead: the outer machine boots from
//! render's own `(expr, table)` and runs that fragment to completion, and a
//! SUBSEQUENT loop fragment carrying a real `runLLMTurn` call then runs on
//! that SAME machine and suspends correctly at its hole. `add_function`'s
//! ConTags re-resolution (`jit_machine.rs` ~1940-1956) still carries a named
//! `log::info!` breadcrumb on the `Err -> Ok` transition as a regression
//! guard, but — per the correction above — that transition is not expected
//! to fire on this path, so it is not what this test asserts.
//!
//! ## This is ALSO a cross-lane regression guard — do not delete as redundant
//!
//! `render`'s own reachable Core only directly builds `Val` (via `pure`); the
//! other four freer scaffolding constructors (`E`/`Union`/`Leaf`/`Node`) end
//! up in its compiled table via `collectTransitiveDCons`
//! (`haskell/src/Tidepool/Translate.hs` 1092-1129, traced directly, not
//! `collectDataCons` at ~2718 as an earlier note here mis-stated): it walks
//! the TYPE closure of every binder's type (`Eff` -> `Val`/`E` -> `E`'s field
//! types `Union effs b` / `FTCQueue (Eff effs) b a`), so all five arrive
//! because `render`'s binder is `Eff`-typed, independent of which
//! constructors its own expression syntactically touches. A parallel lane's
//! D2 item touches this closure computation; as originally specified, a
//! version of that change would have stripped four of the five and silently
//! broken exactly this boot path. D2 now carries a correctness requirement to
//! keep the five freer constructors reachable regardless — and THIS TEST is
//! what catches it if that requirement regresses, because it is deliberately
//! placed in `tidepool-harness/tests/acceptance_*.rs`, which
//! `scripts/battery-shard.sh tidepool-harness -E 'binary(/^acceptance_/)'`
//! runs — that gate is in D2's own mandatory gate set. A guard sitting
//! anywhere else (e.g. `tidepool-runtime/tests/`) would not be in that lane's
//! path and would catch nothing.

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

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

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

/// The step-6 pin: `Harness::new` and `Harness::force` register a node
/// without paying any GHC compile or bringing up a machine — `heap_stats`
/// reads `None` right after `force`. The node's FIRST real turn is what
/// bootstraps the machine — `heap_stats` reads `Some` right after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_machine_after_force_a_machine_after_the_first_turn() {
    if !extract_available() {
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
    // `Harness::new` itself pays no GHC compile any more (both boot seeds are
    // gone) — this constructs instantly even without `TIDEPOOL_EXTRACT`, but
    // the rest of this test needs it, hence the guard above.
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
/// a scripted `finalize` reply — no failure injection, unlike
/// `selfharness_lifecycle.rs`'s `FlakyProvider`. This test's whole point is a
/// completely ordinary first cycle.
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
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

/// The ConTags-leg pin (see this file's module doc for why the invariant
/// changed from the spec's first draft): a FRESH `SelfHarnessDriver`'s outer
/// session bootstraps lazily (`SelfHarnessDriver::bootstrap` ->
/// `crate::harness::Session::unbootstrapped`, no GHC compile of its own), so
/// its machine comes up on `render_framing`'s pure compile — the outer
/// session's first REAL fragment. A single ordinary cycle then drives `loop`
/// (a REAL `runLLMTurn` call) on that SAME machine to a suspend, services the
/// hole through a real nested Agent turn, and resumes to `finalize` — proving
/// the machine that booted off a pure fragment correctly classifies and
/// dispatches a genuinely effectful one afterward.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_session_boots_from_pure_render_then_loop_suspends_on_a_real_hole() {
    if !extract_available() {
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
    assert_eq!(
        cycle.state_json.get("loopCount").and_then(|v| v.as_i64()),
        Some(1),
        "the first-ever cycle must run loop exactly once, got {:?}",
        cycle.state_json
    );
}
