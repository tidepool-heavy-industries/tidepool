//! Item 0's live-shaped receipt (`plans/post-restart/extract-wave/boot/00-spec.md`):
//! not "it should have dropped" but a test that drives the self-iterating
//! harness from launch to the FIRST model call and asserts how many
//! `tidepool-extract` compiles were paid before that call, against a single
//! named constant a later dev flips when the fix lands. Own binary, own
//! process — [`tidepool_harness::compile::extract_spawn_count`] is
//! PROCESS-GLOBAL (nextest already gives one process per test binary, so
//! this test's count is never polluted by another test's compiles).
//!
//! # Traced call chain (the ONE extract-spawn site the self-harness launch
//! path reaches before the first model call — see this crate's `compile.rs`
//! module doc and the counter's doc comment in `tidepool-extract-cmd` for the
//! full site survey)
//!
//! The counter sees EVERY `tidepool-extract` spawn in the process, not just
//! this crate's: it lives in `tidepool_extract_cmd`, which owns the one
//! builder every site goes through (`plans/post-restart/extract-manifest.md`,
//! D-B — before that, spawns through `tidepool_runtime` were invisible here).
//! The constant below is unaffected, because the other sites are not on the
//! pre-model path: the ONE pre-model compile below funnels through
//! [`tidepool_harness::compile::compile_turns`]
//! (`tidepool-harness/src/compile.rs`, at its single `cmd.run()` call) — the
//! harness's turn-compile path is deliberately independent of
//! `tidepool_runtime::compile_haskell`/`cache.rs` (the MCP eval path, never
//! reached from the self-harness driver) and of
//! `tidepool_runtime::session::turn.rs`'s `run_turn`/`classify_block`/
//! `compile_session_turn` (reached only via `Harness::run_block`, i.e. only
//! AFTER a model turn produces a Haskell block to run — never before the
//! first model call). The two eager boot seeds (`Harness::new`'s and
//! `SelfHarnessDriver::bootstrap`'s trivial ConTags-seeding compiles) are
//! ALREADY GONE (lazy boot, extract-wave item 0 steps 1-3); wave-3's
//! render+loop fusion removes the remaining split:
//!
//! 1. `SelfHarnessDriver::run_one_cycle` →
//!    `SelfHarnessDriver::compile_cycle_entry` (driver.rs) — ONE
//!    `tidepool-extract` spawn (`compile::compile_turns`, two `--targets`
//!    over one shared merged `meta.cbor`) compiling the pre-loop
//!    `render(state, lastCompaction)` and this cycle's
//!    `loop __selfHarnessState` TOGETHER, as distinct top-level entries of
//!    ONE module. Only once this compile succeeds and the loop suspends on
//!    its first `runLLMTurn` hole does the driver ever call a model —
//!    `service_runllm_hole` → `drive_answerer_to_finalize` →
//!    `Harness::drive_turn` → `engine::drive_model_turn` (the first live
//!    [`ModelProvider::complete`] call, which [`SnapshotOnFirstCall`] below
//!    intercepts).
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`). Pays 1 real GHC extract compile
//! — sized to fit comfortably inside this environment's ~380s shard budget
//! as its own binary: `export XDG_CACHE_HOME="$PWD/.cache" &&
//! scripts/battery-shard.sh tidepool-harness -E
//! 'binary(acceptance_boot_compile_count)'`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

mod support;

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
/// originally paid FOUR `tidepool-extract` compiles before the first model
/// call — two trivial boot seeds (the outer session's, `driver.rs::bootstrap`,
/// and the answerer `Harness`'s own, `harness.rs::Harness::new`) purely to
/// seed ConTags, plus `compile_outer` of the render framing and
/// `compile_outer` of the loop body.
///
/// Item 0's target end-state (`plans/post-restart/extract-wave/boot/00-spec.md`)
/// is exactly **1**: delete both boot seeds, and fuse render+loop emission
/// into one extract invocation. Both steps have now landed.
///
/// Harness turn compiles are memoized as of `plans/compile-memo.md`, so
/// "clean cache" is now enforced by the test (`support::isolate_compile_memo`)
/// rather than assumed of the ambient environment — a warm memo pays 0 spawns,
/// which would be a receipt about cache state, not about the boot path.
///
/// MEASURED 2026-08-09 on the centralized tip (this suite, clean cache): 2 —
/// both boot seeds gone (item 0 steps 1-3, boot-lazy), leaving exactly the
/// two `compile_outer` invocations (render framing + loop body) that wave 3's
/// render+loop fusion targeted.
///
/// MEASURED 2026-08-11 on this branch (this suite, clean cache): **1** —
/// `SelfHarnessDriver::compile_cycle_entry` (driver.rs) now compiles the
/// pre-loop render and this cycle's loop body as two entries of ONE module in
/// ONE `tidepool-extract` spawn (wave-3 render+loop fusion,
/// `plans/post-restart/extract-wave/spawn-latency/04-turn-latency-plan.md`
/// §2). Item 0's target end-state is reached.
pub const PRE_MODEL_EXTRACT_COMPILES: u64 = 1;

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
    support::require_extract();

    // This receipt is about the BOOT PATH, not about cache state. Harness turn
    // compiles are memoized (`plans/compile-memo.md`), so against the shared
    // test memo this counts 0 spawns on a warm run and
    // `PRE_MODEL_EXTRACT_COMPILES` on a cold one. The constant's own doc says
    // "clean cache"; this enforces it instead of assuming it.
    let _memo_guard = support::isolate_compile_memo();

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
    let outcome = driver.run_one_cycle(&source, None).await;
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
