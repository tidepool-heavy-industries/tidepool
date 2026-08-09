//! Item 0's live-shaped receipt (`plans/post-restart/extract-wave/boot/00-spec.md`):
//! not "it should have dropped" but a test that drives the self-iterating
//! harness from launch to the FIRST model call and asserts how many
//! `tidepool-extract` compiles were paid before that call, against a single
//! named constant a later dev flips when the fix lands. Own binary, own
//! process — [`tidepool_harness::compile::extract_spawn_count`] is
//! PROCESS-GLOBAL (nextest already gives one process per test binary, so
//! this test's count is never polluted by another test's compiles).
//!
//! # Traced call chain (every extract-spawn site the self-harness launch
//! path reaches before the first model call — see this crate's `compile.rs`
//! module doc and `EXTRACT_SPAWNS`' doc comment for the full site survey)
//!
//! All FOUR pre-model compiles below funnel through the ONE spawn function
//! [`tidepool_harness::compile::compile_turn`] (`tidepool-harness/src/compile.rs`,
//! at its single `cmd.output()` call) — the harness's turn-compile path is
//! deliberately independent of `tidepool_runtime::compile_haskell`/
//! `cache.rs` (the MCP eval path, never reached from the self-harness
//! driver) and of `tidepool_runtime::session::turn.rs`'s `run_turn`/
//! `classify_block`/`compile_session_turn` (reached only via
//! `Harness::run_block`, i.e. only AFTER a model turn produces a Haskell
//! block to run — never before the first model call):
//!
//! 1. `Harness::new` (`tidepool-harness/src/harness.rs` ~430) — the answerer
//!    `Harness`'s own boot seed, `pure (toJSON (0 :: Int))` compiled to seed
//!    the answerer stack's ConTags, paid before `Harness::new` even returns
//!    (i.e. before this test constructs its `agent`).
//! 2. `SelfHarnessDriver::bootstrap` (`tidepool-harness/src/selfharness/driver.rs`
//!    ~570) — the SAME trivial compile, seeding the outer `Eff
//!    '[RunLLMTurn, AskUser]` session's ConTags.
//! 3. `SelfHarnessDriver::render_framing` → `compile_outer` (driver.rs
//!    ~1507) — the pre-loop `render` framing.
//! 4. `SelfHarnessDriver::run_loop_fragment_inner` → `compile_outer`
//!    (driver.rs ~929) — `Loaded.loop __selfHarnessState`. Only once THIS
//!    compile succeeds and the loop suspends on its first `runLLMTurn` hole
//!    does the driver ever call a model — `service_runllm_hole` →
//!    `drive_answerer_to_finalize` → `Harness::drive_turn` →
//!    `engine::drive_model_turn` (the first live [`ModelProvider::complete`]
//!    call, which [`SnapshotOnFirstCall`] below intercepts).
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`). Pays 4 real GHC extract compiles
//! (~15-30s each, ~96s total per the D7 measurement below) — sized to fit
//! inside this environment's ~380s shard budget as its own binary:
//! `export XDG_CACHE_HOME="$PWD/.cache" && scripts/battery-shard.sh
//! tidepool-harness -E 'binary(acceptance_boot_compile_count)'`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tidepool_harness::compile;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{LogHeader, LogWriter};
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse,
};
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver,
};

/// The D7 live-dogfood measurement (2026-08-08, clean cache,
/// `plans/post-restart/extract-wave.md`): a clean-cache self-harness launch
/// pays FOUR `tidepool-extract` compiles before the first model call — two
/// trivial boot seeds (the outer session's, `driver.rs::bootstrap`, and the
/// answerer `Harness`'s own, `harness.rs::Harness::new`) purely to seed
/// ConTags, plus `compile_outer` of the render framing and `compile_outer`
/// of the loop body.
///
/// Item 0's target end-state (`plans/post-restart/extract-wave/boot/00-spec.md`)
/// is exactly **1**: delete both boot seeds, and fuse render+loop emission
/// into one extract invocation. A later dev lands the fix by changing THIS
/// constant and nothing else in this test — the test itself, and the
/// spawn-counting instrumentation it asserts against, stay unchanged.
pub const PRE_MODEL_EXTRACT_COMPILES: u64 = 4;

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> PathBuf {
    repo_root().join("haskell/lib")
}

fn examples_harness_dir() -> PathBuf {
    repo_root().join("examples/harness")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "acceptance-boot-compile-count".into(),
        extract_fingerprint: "acceptance-boot-compile-count".into(),
        harness_version: "test".into(),
    }
}

/// A [`ModelProvider`] whose FIRST invocation snapshots the process-global
/// [`compile::extract_spawn_count`] into `snapshot`, then returns an error
/// that cleanly terminates the self-harness cycle — the pre-model spawn
/// count is already captured by the time the error unwinds, so a canned
/// success reply is unnecessary (and would need a full `finalize @Decision`
/// round-trip this receipt does not need). Every invocation past the first
/// is a no-op on `snapshot` (guarded by `is_none`), though nothing in this
/// test should ever reach a second call — the driver surfaces the error and
/// stops.
struct SnapshotOnFirstCall {
    snapshot: Mutex<Option<u64>>,
}

impl SnapshotOnFirstCall {
    fn new() -> Self {
        SnapshotOnFirstCall {
            snapshot: Mutex::new(None),
        }
    }
}

impl ModelProvider for SnapshotOnFirstCall {
    async fn complete(
        &self,
        _req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let mut snap = self.snapshot.lock().unwrap();
        if snap.is_none() {
            *snap = Some(compile::extract_spawn_count());
        }
        Err(ProviderError::Api(
            "acceptance_boot_compile_count: snapshot captured, terminating the cycle by design"
                .into(),
        ))
    }
}

/// Drive the self-iterating harness from `Harness::new` (the answerer boot
/// compile) through the FIRST live model call, and assert the number of
/// `tidepool-extract` compiles paid before that call against
/// [`PRE_MODEL_EXTRACT_COMPILES`].
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_pays_pre_model_extract_compiles_matching_baseline() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    // Own process (nextest gives every test binary its own), but reset
    // anyway: this test binary has exactly one test function, so this only
    // guards against a future second test landing in this file and sharing
    // the process-global counter unexpectedly.
    compile::reset_extract_spawn_count();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");

    let stub = Arc::new(SnapshotOnFirstCall::new());
    let provider: Arc<dyn DynModelProvider> = stub.clone();

    let writer = LogWriter::create(
        std::env::temp_dir().join(format!(
            "acceptance-boot-compile-count-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    // Compile #1 (the answerer boot seed) is paid inside this call.
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    // Compiles #2 (outer boot seed), #3 (render), #4 (loop) are paid inside
    // this call, in that order, before the loop suspends on its first
    // `runLLMTurn` hole and the driver calls the model for the first time
    // (which `SnapshotOnFirstCall` intercepts and snapshots, then errors to
    // cleanly end the cycle).
    let outcome = driver.run_one_cycle(&source, None);
    assert!(
        outcome.is_err(),
        "SnapshotOnFirstCall always errors its first (and only expected) call — \
         a Ok(..) outcome means the driver never reached the model, which would make \
         this receipt's snapshot untrustworthy: {outcome:?}"
    );

    let snapshot = stub.snapshot.lock().unwrap().expect(
        "the model provider must have been called at least once for this receipt to mean anything",
    );

    assert_eq!(
        snapshot, PRE_MODEL_EXTRACT_COMPILES,
        "pre-model extract-spawn count changed: observed {snapshot}, expected {PRE_MODEL_EXTRACT_COMPILES} \
         (plans/post-restart/extract-wave/boot/00-spec.md). A LOWER observed count than expected is the \
         wave's win condition — if that's what you see, update PRE_MODEL_EXTRACT_COMPILES in this file to \
         match and nothing else. A HIGHER count is a regression: something now pays an extra compile before \
         the first model call."
    );
}
