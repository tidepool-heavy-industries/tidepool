//! End-to-end failure-isolation tests for the `try*` verbs.
//!
//! These drive the FULL pipeline the live MCP server uses — Haskell source
//! (preamble + generated `Tidepool.Effects` module) → extract → JIT → effect
//! dispatch — with a dispatcher that errors on demand, and assert the four
//! load-bearing behaviors:
//!
//!  1. A handler `EffectError::Handler` inside a `try*` verb becomes `Left err`
//!     (carrying the cause) and the eval CONTINUES to its pure result.
//!  2. Success inside a `try*` verb is `Right v`.
//!  3. The SAME failing effect WITHOUT `try` aborts the eval (current behavior
//!     unchanged — the catch is opt-in, never implicit).
//!  4. A non-Handler (structural/`Bridge`) error is NEVER swallowed into a
//!     `Left`, even through a `try*` verb — the line between failure ISOLATION
//!     and corruption HIDING.
//!
//! Harness shape mirrors `repro_qq_union.rs`. The dispatcher ignores the tag
//! (each eval fires exactly one effect) and acts per a fixed `Mode`, so the
//! tests exercise `EffectContext::respond_caught` over the real heap/bridge
//! round-trip rather than the network/exec handlers.

use std::path::Path;
use tidepool_effect::error::EffectError;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::{compile_and_run, EvalResult};

/// How the one effect this eval fires should be answered.
#[derive(Clone)]
enum Mode {
    /// Catchable external failure → `respond_caught(Err(Handler(msg)))` → Left.
    CaughtLeft(String),
    /// Success → `respond_caught(Ok(json))` → Right.
    CaughtRight(serde_json::Value),
    /// Raw handler error, NOT routed through a try verb → aborts the eval.
    RawHandlerErr(String),
    /// Structural error through `respond_caught` → must propagate, not Left.
    CaughtStructural,
}

struct ModeDispatcher {
    mode: Mode,
}

impl DispatchEffect<()> for ModeDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match &self.mode {
            Mode::CaughtLeft(msg) => cx.respond_caught(Err::<serde_json::Value, _>(
                EffectError::Handler(msg.clone()),
            )),
            Mode::CaughtRight(json) => cx.respond_caught(Ok::<serde_json::Value, _>(json.clone())),
            Mode::RawHandlerErr(msg) => Err(EffectError::Handler(msg.clone())),
            Mode::CaughtStructural => cx.respond_caught(Err::<serde_json::Value, _>(
                EffectError::Bridge(tidepool_bridge::BridgeError::UnknownDataConName(
                    "synthetic-structural-error".into(),
                )),
            )),
        }
    }
}

/// Compile + run `code` (a `do`-block body) against the standard MCP effect
/// stack with the given dispatcher mode. Returns the eval result or error.
fn run_eval(code: &str, mode: Mode) -> Result<EvalResult, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(code),
        "",
        "",
        None,
        None,
    );
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib").leak() as &Path;
    let lib = root.join(".tidepool/lib").leak() as &Path;
    let include = [hs, lib, effects_dir];
    let mut d = ModeDispatcher { mode };
    compile_and_run(&src, "result", &include, &mut d, &()).map_err(|e| e.to_string())
}

/// Run `f` on a 64MB-stack thread (the standard effectful-test harness: the
/// generated table + JIT setup can exhaust the default 2MB test stack).
fn on_big_stack<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

/// (1) A handler failure inside `tryHttpGet` becomes `Left err` and the eval
/// continues — AND the Left text carries the operation context verbatim.
#[test]
fn try_caught_left_continues() {
    on_big_stack(|| {
        let msg = "HTTP GET 'http://probe.invalid/x' failed: simulated 404";
        let code = "r <- tryHttpGet \"http://probe.invalid/x\"\n\
                    case r of { Left e -> pure (\"caught: \" <> e); Right _ -> pure \"unexpected-ok\" }";
        let v = run_eval(code, Mode::CaughtLeft(msg.to_string()))
            .expect("eval must SURVIVE a caught failure and return a pure result");
        let got = v.to_json();
        let s = got.as_str().expect("result is a JSON string");
        assert!(
            s.starts_with("caught: "),
            "expected the Left branch to run, got {s:?}"
        );
        // Team-lead #5: the Left text must carry operation context.
        assert!(
            s.contains("http://probe.invalid/x") && s.contains("simulated 404"),
            "Left text must carry the URL + cause, got {s:?}"
        );
    });
}

/// (2) Success inside a `try*` verb is `Right v`.
#[test]
fn try_right_on_success() {
    on_big_stack(|| {
        let code = "r <- tryHttpGet \"http://probe.invalid/ok\"\n\
                    case r of { Right _ -> pure \"ok-branch\"; Left e -> pure (\"left: \" <> e) }";
        let v = run_eval(code, Mode::CaughtRight(serde_json::json!({"ok": true})))
            .expect("eval must succeed");
        assert_eq!(v.to_json(), serde_json::json!("ok-branch"));
    });
}

/// (3) The SAME failing effect WITHOUT `try` aborts the eval — the catch is
/// opt-in; plain `httpGet` keeps the pre-existing eval-killing behavior.
#[test]
fn untried_effect_aborts_eval() {
    on_big_stack(|| {
        let code = "v <- httpGet \"http://probe.invalid/x\"\n\
                    pure (maybe (0 :: Int) round (v ^? _Number))";
        let r = run_eval(
            code,
            Mode::RawHandlerErr("HTTP GET 'http://probe.invalid/x' failed: simulated".into()),
        );
        assert!(
            r.is_err(),
            "an untried failing effect must still abort the eval, got {r:?}"
        );
        let e = r.unwrap_err();
        assert!(
            e.contains("simulated"),
            "the abort error must carry the handler text, got {e:?}"
        );
    });
}

/// (4) A structural (`Bridge`) error is NEVER swallowed into a `Left`, even
/// through a `try*` verb — failure ISOLATION must not become corruption
/// HIDING. The eval aborts, carrying the structural error.
#[test]
fn try_does_not_swallow_structural_error() {
    on_big_stack(|| {
        let code = "r <- tryHttpGet \"http://probe.invalid/x\"\n\
                    case r of { Left e -> pure e; Right _ -> pure \"ok\" }";
        let r = run_eval(code, Mode::CaughtStructural);
        assert!(
            r.is_err(),
            "a structural/Bridge error must propagate (abort), NOT become a Left, got {r:?}"
        );
    });
}
