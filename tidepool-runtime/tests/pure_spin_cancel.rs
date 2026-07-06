//! A pure-CPU spin (no allocation, no effect dispatch) submitted as a real
//! `SessionEngine` turn is interrupted by the turn timeout, not left to hang.
//!
//! The underlying JIT-level safepoints (recursive join back-edge #325, GC
//! trigger #273, effect dispatch, tail-call trampoline) are already covered by
//! `tidepool-codegen/tests/external_cancellation.rs` with hand-built IR. This
//! test instead exercises the layer above: `SessionEngine::start_turn` driving
//! a REAL Haskell program (through the full extract → JIT pipeline) to prove
//! the engine's timeout → pause-request → grace-period → detach-and-cancel
//! wiring (`tidepool-runtime/src/session/engine.rs`'s `drive`) actually flips
//! the compiled machine's `CancelHandle` for a turn that never reaches any
//! effect boundary, mirroring `tidepool-repl`'s
//! `timed_out_runaway_self_heals_to_idle` (H3) at the stateless-eval-server
//! layer instead of the resident-REPL layer.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_effect::{DispatchEffect, Response};
use tidepool_eval::value::Value;
use tidepool_runtime::session::{
    EngineConfig, OutputSink, RenderPolicy, Retention, SessionEngine, StartTurn, TurnOutcome,
};
use tidepool_runtime::FailureClass;

/// Never invoked — the fixture program raises no effects. Present only so the
/// full effectful preamble type-checks (mirrors `jit_surface.rs`'s `NullDispatcher`).
#[derive(Clone)]
struct NullDispatcher;

impl DispatchEffect<TestSink> for NullDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &EffectContext<'_, TestSink>,
    ) -> Result<Response, EffectError> {
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

/// Build a real, fully-wrapped module source for `code` (a single Haskell
/// expression of type `M a`), same wrapping the stateless MCP eval server
/// applies — see `tidepool-mcp/src/server.rs` and `jit_surface.rs`.
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

fn test_engine() -> SessionEngine<TestSink> {
    SessionEngine::new(EngineConfig {
        max_concurrent: 4,
        max_orphaned: 10,
        cont_prefix: "cont".into(),
        default_timeout_secs: 5,
        render: RenderPolicy::Json,
        retention: Retention::DropAfterDone,
    })
}

/// `let go n = go (n + 1 :: Int) in pure (go 0)` — a tail-recursive Int loop
/// GHC's strictness analysis unboxes to a non-allocating Int# worker (the same
/// shape as `tidepool-codegen`'s `build_long_running_countdown` fixture, just
/// produced by the real GHC→Core pipeline instead of hand-built IR). It never
/// terminates and never dispatches an effect, so the ONLY safepoint it can
/// reach is the tail-call trampoline's cancel check.
#[tokio::test]
async fn pure_cpu_spin_is_interrupted_by_turn_timeout() {
    let (source, include) =
        wrapped_source("let { go :: Int -> Int; go n = go (n + 1) } in pure (go 0)");

    let engine = test_engine();
    let start = Instant::now();
    let outcome = match engine
        .start_turn(StartTurn {
            source: source.into(),
            include,
            handlers: NullDispatcher,
            ask_tag: u64::MAX,
            effect_names: Vec::new(),
            captured: TestSink::default(),
            nursery_size: tidepool_runtime::DEFAULT_NURSERY_SIZE,
            // Must exceed the per-eval GHC compile (cold cache can take tens of
            // seconds) so the timeout fires during the RUN phase, not mid-compile.
            timeout_secs: 45,
        })
        .await
    {
        Ok(outcome) => outcome,
        Err(_) => panic!("admission must succeed (fresh engine, empty pool)"),
    };
    let elapsed = start.elapsed();

    match outcome {
        TurnOutcome::TimedOut {
            class, compiling, ..
        } => {
            assert!(
                !compiling,
                "expected a run-phase timeout, not a slow compile"
            );
            assert_eq!(class, FailureClass::Runtime);
        }
        TurnOutcome::Completed { .. } => panic!("fixture loop was not actually infinite"),
        _ => panic!("expected TimedOut, got a different outcome variant"),
    }

    // Generous bound: 45s turn window + up to 2s grace period waiting for an
    // effect checkpoint that never comes, plus scheduling slack.
    assert!(
        elapsed < Duration::from_secs(60),
        "cancellation took {:?}, expected well under 60s",
        elapsed
    );
}
