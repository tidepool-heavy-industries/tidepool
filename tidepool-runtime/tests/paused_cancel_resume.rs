//! F1 regression: a `Paused` continuation's JIT `CancelHandle` and original
//! caller-clamped timeout window must survive `resume`/`abort` — not be
//! silently replaced by a fresh (never-installed) cancel slot or
//! `config.default_timeout_secs`.
//!
//! Before the fix, `SessionEngine::resume`/`abort` re-drove a `Paused`
//! continuation with a BRAND NEW `Arc<Mutex<Option<CancelHandle>>>` that the
//! already-running eval thread never installs into (the `on_ready` callback
//! only fires once, at machine creation, on the ORIGINAL slot). A turn parked
//! at an effect boundary, resumed into a pure loop that never yields again,
//! would then detach on its next timeout with no way to flip the JIT cancel
//! flag — the thread spins forever, holding its semaphore permit, and the
//! reaper's `join()` blocks forever so `orphaned_threads` never returns to 0.
//!
//! This test drives the real `SessionEngine` through exactly that sequence —
//! real GHC → JIT compile, a genuine parked continuation, a real resume — and
//! asserts the runaway is actually cancelled and reaped.
//!
//! Requires `TIDEPOOL_EXTRACT` (GHC→Core extractor) on the environment, same
//! as the other GHC-heavy `tidepool-runtime` tests (see `pure_spin_cancel.rs`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_effect::{DispatchEffect, Response};
use tidepool_eval::value::Value;
use tidepool_runtime::session::{
    EngineConfig, OutputSink, RenderPolicy, ResumeOutcome, Retention, SessionEngine, StartTurn,
    TurnOutcome,
};
use tidepool_runtime::FailureClass;

/// Blocks the FIRST effect dispatch until the test flips `release` — a
/// controllable stand-in for a slow external call — then answers every
/// dispatch with a throwaway value (the fixture program never inspects the
/// response, mirroring `pure_spin_cancel.rs`'s `NullDispatcher`).
struct BlockFirstDispatcher {
    release: Arc<AtomicBool>,
    fired: bool,
}

impl DispatchEffect<TestSink> for BlockFirstDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &EffectContext<'_, TestSink>,
    ) -> Result<Response, EffectError> {
        if !self.fired {
            self.fired = true;
            while !self.release.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        cx.respond(serde_json::json!(0))
    }
}

#[derive(Clone, Default)]
struct TestSink {
    lines: Arc<Mutex<Vec<String>>>,
}

impl OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock().unwrap())
    }
    fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

/// Build a real, fully-wrapped module source for `code` — same wrapping the
/// stateless MCP eval server applies (see `pure_spin_cancel.rs`).
fn wrapped_source(code: &str) -> (String, Vec<std::path::PathBuf>) {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, "", "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    (src, vec![hs, lib, effects_dir])
}

fn test_engine(default_timeout_secs: u64) -> SessionEngine<TestSink> {
    SessionEngine::new(EngineConfig {
        max_concurrent: 4,
        max_orphaned: 10,
        cont_prefix: "cont".into(),
        // Deliberately different from the turn's own clamped window: a
        // fall-back-to-default bug (the F1 "smaller items" reset) would show
        // up as a `TimedOut.timeout_secs` of 3, not the turn's 45.
        default_timeout_secs,
        render: RenderPolicy::Json,
        retention: Retention::DropAfterDone,
    })
}

/// Must exceed a cold-cache GHC compile (can take tens of seconds) so the
/// FIRST timeout fires during the RUN phase — while still blocked in the
/// effect dispatch — not mid-compile. Mirrors `pure_spin_cancel.rs`.
const TURN_TIMEOUT: u64 = 45;

fn describe(o: &TurnOutcome) -> String {
    match o {
        TurnOutcome::Completed { result, .. } => format!("Completed({result})"),
        TurnOutcome::SuspendedAsk { prompt, .. } => format!("SuspendedAsk({prompt})"),
        TurnOutcome::Paused { .. } => "Paused".into(),
        TurnOutcome::Error { detail, .. } => format!("Error({detail})"),
        TurnOutcome::TimedOut { .. } => "TimedOut".into(),
        TurnOutcome::Crashed { thread_panic, .. } => format!("Crashed({thread_panic:?})"),
    }
}

fn describe_resume(o: &ResumeOutcome<String>) -> String {
    match o {
        ResumeOutcome::NotFound => "NotFound".into(),
        ResumeOutcome::Invalid(v) => format!("Invalid({v})"),
        ResumeOutcome::ThreadGone => "ThreadGone".into(),
        ResumeOutcome::Driven(o) => format!("Driven({})", describe(o)),
    }
}

/// Park a turn Paused at a (blocked) effect boundary, resume it into a pure
/// infinite loop that never dispatches another effect, and confirm the
/// resumed runaway is cancelled — not left pinning its thread and permit.
#[tokio::test]
async fn paused_resume_into_runaway_is_cancelled_and_reaped() {
    let (source, include) = wrapped_source(
        "say \"go\" >> (let { runaway :: Int -> Int; runaway n = runaway (n + 1) } in \
         pure (runaway (0 :: Int)))",
    );

    let engine = test_engine(3);
    let release = Arc::new(AtomicBool::new(false));
    let handlers = BlockFirstDispatcher {
        release: Arc::clone(&release),
        fired: false,
    };

    let cont_id = match engine
        .start_turn(StartTurn {
            source: source.into(),
            include,
            handlers,
            ask_tag: u64::MAX,
            effect_names: Vec::new(),
            captured: TestSink::default(),
            nursery_size: tidepool_runtime::DEFAULT_NURSERY_SIZE,
            timeout_secs: TURN_TIMEOUT,
        })
        .await
    {
        Ok(TurnOutcome::Paused {
            cont_id,
            timeout_secs,
            ..
        }) => {
            assert_eq!(timeout_secs, TURN_TIMEOUT);
            cont_id
        }
        Ok(other) => panic!(
            "expected Paused (still blocked in the effect dispatch), got {}",
            describe(&other)
        ),
        Err(_) => panic!("admission must succeed (fresh engine, empty pool)"),
    };

    // Unblock the effect right as we resume: the dispatch completes almost
    // immediately, the program falls into the never-yielding `runaway` loop,
    // and THIS resumed drive must also see it out to a cancelled TimedOut.
    release.store(true, Ordering::Relaxed);

    let outcome = engine
        .resume::<_, String>(&cont_id, |_schema| Ok(serde_json::json!(null)))
        .await;

    match outcome {
        ResumeOutcome::Driven(TurnOutcome::TimedOut {
            class,
            compiling,
            timeout_secs,
            ..
        }) => {
            assert!(
                !compiling,
                "expected a run-phase timeout, not a slow compile"
            );
            assert_eq!(class, FailureClass::Runtime);
            assert_eq!(
                timeout_secs, TURN_TIMEOUT,
                "resume must re-drive with the turn's ORIGINAL caller-clamped window, \
                 not config.default_timeout_secs"
            );
        }
        other => panic!(
            "expected Driven(TimedOut) — a resumed runaway must be detached, not hang; got {}",
            describe_resume(&other)
        ),
    }

    // Permit accounting (F1's actual regression guard): without the fix, the
    // fresh empty cancel slot never flips the JIT cancel flag, the detached
    // thread spins forever, and the reaper's `join()` blocks forever — this
    // bounded poll (not an unbounded wait) turns that hang into a clean
    // assertion failure instead of a stuck test process.
    let deadline = Instant::now() + Duration::from_secs(15);
    while engine.orphaned_count() > 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        engine.orphaned_count(),
        0,
        "orphaned thread was never reaped — the resumed runaway's cancel handle \
         was not observed (fresh cancel_slot bug)"
    );
}
