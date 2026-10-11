//! Print continuations run through the real GHC→JIT pipeline with one Console
//! handler. A pure continuation exercises GHC's nospec specialization wrapper;
//! a failing continuation must dispatch Print before its language failure.

use std::path::PathBuf;

use tidepool_bridge_derive::FromHaskell;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;
use tidepool_runtime::{compile_and_run, RuntimeError};

fn prelude_dir() -> PathBuf {
    PathBuf::from(
        std::env::var_os("TIDEPOOL_PRELUDE_DIR")
            .expect("TIDEPOOL_PRELUDE_DIR must name the declared Haskell library resource"),
    )
}

#[derive(FromHaskell)]
enum ConsoleReq {
    #[haskell(name = "Print")]
    Print(String),
}

#[derive(Clone)]
struct ConsoleHandler;

impl EffectHandler<CapturedOutput> for ConsoleHandler {
    type Request = ConsoleReq;
    fn handle(
        &mut self,
        req: ConsoleReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            ConsoleReq::Print(s) => {
                cx.user().push(s);
                cx.respond(())
            }
        }
    }
}

/// Run `code` with one Console handler, retaining compiler closure observations
/// and preserving typed execution failures.
fn run(code: &str) -> (Vec<String>, Result<serde_json::Value, RuntimeError>) {
    let decls = [tidepool_mcp::console_decl()];
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None);

    let pp = prelude_dir();
    let dirs = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let include = [
        pp.as_path(),
        dirs.core.as_path(),
        dirs.orchestration.as_path(),
    ];

    let captured = CapturedOutput::new();
    let mut handlers = frunk::hlist![ConsoleHandler];
    let out = tidepool_testing::with_settlement(|settlement| {
        compile_and_run(
            &source,
            "result",
            &include,
            &mut handlers,
            &captured,
            settlement,
        )
    })
    .map(|result| result.to_json());
    (captured.drain(), out)
}

#[test]
fn pure_only_ok() {
    let (out, r) = run("pure (123 :: Int)");
    assert_eq!(
        r.expect("pure (123) should succeed"),
        serde_json::json!(123)
    );
    assert!(out.is_empty(), "pure must not dispatch Print: {out:?}");
}

#[test]
fn print_then_error_resolves() {
    let (out, r) =
        run(r#"send (Print (T.pack "MARK")) >> (error (T.pack "boom") :: Eff '[Console] Int)"#);
    assert!(
        matches!(&r, Err(RuntimeError::Prepared(error))
            if error.kind() == tidepool_runtime::session::PreparedFailureKind::Language),
        "expected a language failure after Print, got {r:?}"
    );
    assert_eq!(out, ["MARK"]);
}

#[test]
fn print_then_pure_resolves() {
    let (out, r) = run(r#"send (Print (T.pack "MARK")) >> (pure (123 :: Int))"#);
    assert_eq!(
        r.expect("send (Print) >> pure should succeed"),
        serde_json::json!(123)
    );
    assert_eq!(out, ["MARK"]);
}

#[test]
fn print_then_strict_pure_resolves() {
    let (out, r) = run(r#"send (Print (T.pack "MARK")) >> (pure $! (123 :: Int))"#);
    assert_eq!(
        r.expect("send (Print) >> strict pure should succeed"),
        serde_json::json!(123)
    );
    assert_eq!(out, ["MARK"]);
}
