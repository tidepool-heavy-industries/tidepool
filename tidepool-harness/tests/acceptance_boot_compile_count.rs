//! Pins the number of extractor invocations a self-harness pays before its
//! first model call. The process-global counter observes both pre-model paths:
//! `refresh_harness_ctx` installs the live context through `run_turn`, then
//! `compile_loop_entry` compiles the fused render/loop artifact. Boot itself
//! remains lazy. Run inside the repository's Nix/GHC environment.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

mod support;

use tidepool_harness::engine;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{LogHeader, LogWriter};
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse,
};
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Harness, LogObserver, SelfHarnessDriver,
};

/// A clean-cache self-harness launch pays exactly **2** `tidepool-extract`
/// compiles before the first model call:
///
/// 1. `SelfHarnessDriver::refresh_harness_ctx`'s small resident bind turn
///    (compiling a tiny `(Text, Text)` tuple literal, `Data.Text`-only) that
///    injects the stable val — deliberately never memo-cacheable (fresh
///    literal content each time), paid every cycle.
/// 2. `SelfHarnessDriver::compile_loop_entry`'s fused compile of the pre-loop
///    render and this cycle's loop body as two entries of ONE module in ONE
///    `tidepool-extract` spawn.
///
/// That second compile is BYTE-IDENTICAL turn to turn once the stable-val
/// injection precedes it, so every cycle after the first is a memo HIT for it
/// — see `tests/state_injection_memo_hit.rs`, which pins that behavior.
///
/// Harness turn compiles are memoized, so "clean cache" is enforced by the
/// test (`support::isolate_compile_memo`) rather than assumed of the ambient
/// environment — a warm memo pays 0 spawns, which would be a receipt about
/// cache state, not about the boot path.
pub const PRE_MODEL_EXTRACT_COMPILES: u64 = 2;

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
/// [`engine::extract_spawn_count`] into `snapshot`, then returns an error
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
        let mut snap = self.snapshot.lock();
        if snap.is_none() {
            *snap = Some(engine::extract_spawn_count());
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
    support::require_extract();

    // This receipt is about the BOOT PATH, not about cache state. Harness turn
    // compiles are memoized, so against the shared
    // test memo this counts 0 spawns on a warm run and
    // `PRE_MODEL_EXTRACT_COMPILES` on a cold one. The constant's own doc says
    // "clean cache"; this enforces it instead of assuming it.
    let _memo_guard = support::isolate_compile_memo();

    // Own process (nextest gives every test binary its own), but reset
    // anyway: this test binary has exactly one test function, so this only
    // guards against a future second test landing in this file and sharing
    // the process-global counter unexpectedly.
    engine::reset_extract_spawn_count();

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
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
    let outcome = driver.run_one_loop_iteration(&source, None).await;
    assert!(
        outcome.is_err(),
        "SnapshotOnFirstCall always errors its first (and only expected) call — \
         a Ok(..) outcome means the driver never reached the model, which would make \
         this receipt's snapshot untrustworthy: {outcome:?}"
    );

    let snapshot = stub.snapshot.lock().expect(
        "the model provider must have been called at least once for this receipt to mean anything",
    );

    assert_eq!(
        snapshot, PRE_MODEL_EXTRACT_COMPILES,
        "pre-model extract-spawn count changed: observed {snapshot}, expected {PRE_MODEL_EXTRACT_COMPILES}. \
         A LOWER observed count than expected is an improvement \
         — if that's what you see, update PRE_MODEL_EXTRACT_COMPILES in this file to \
         match and nothing else. A HIGHER count is a regression: something now pays an extra compile before \
         the first model call."
    );
}
