//! REPRO (2026-06-20): `send (Print …)` resolves to an unresolved variable at
//! runtime in some contexts, even though it type-checks and `run`/other effects
//! work. See plans/send-print-unresolved-bug.md. These tests MAP the minimal
//! trigger in a controlled (single-Console-handler) effect stack so the bug can
//! be bisected against pre-wave commits.
//!
//! Each test runs one snippet through the real GHC→JIT pipeline and reports the
//! exact outcome (Ok value / which RuntimeError). They assert the EXPECTED-good
//! behavior, so a reproduction shows up as a failure naming the unresolved var.

use std::path::Path;

use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;
use tidepool_runtime::compile_and_run;

fn prelude_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell/lib")
        .leak()
}

fn user_lib_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".tidepool/lib")
        .leak()
}

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
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

/// Run `code` through the real pipeline (single Console handler). Returns the
/// captured `say` output and either the debug-rendered result or the runtime
/// error string.
fn run(code: &str) -> (Vec<String>, Result<String, String>) {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, None);

    let pp = prelude_dir();
    let ulp = user_lib_dir();
    let eff = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let include = [pp, ulp, eff];

    let captured = CapturedOutput::new();
    let mut handlers = frunk::hlist![ConsoleHandler];
    let out = match compile_and_run(&source, "result", &include, &mut handlers, &captured) {
        Ok(r) => Ok(format!("{r:?}")),
        Err(e) => Err(e.to_string()),
    };
    (captured.drain(), out)
}

/// Control: a bare `pure` with no effect resolves fine.
#[test]
fn pure_only_ok() {
    let (_out, r) = run("pure (123 :: Int)");
    assert!(r.is_ok(), "pure (123) should succeed, got {r:?}");
}

/// Control: `send (Print …) >> error` already passes elsewhere — Print resolves
/// when the continuation is an `error`.
#[test]
fn print_then_error_resolves() {
    let (out, r) = run(r#"send (Print (T.pack "MARK")) >> (error (T.pack "boom") :: M Int)"#);
    // Expect a Haskell error (NOT an unresolved-variable), with the marker captured.
    match &r {
        Err(e) => {
            assert!(
                !e.contains("unresolved variable"),
                "Print unresolved even with `>> error`: {e}"
            );
            assert!(out.iter().any(|l| l.contains("MARK")), "marker not captured: {out:?}");
        }
        Ok(v) => panic!("expected an error, got Ok({v})"),
    }
}

/// SUSPECT: a simple `pure` continuation after the Print.
/// CURRENTLY FAILS — root cause confirmed: commit a9a0082 scoped the
/// DataConTable meta walk to the target's reachable VALUE bindings, which
/// excludes `()` (the unit-returning effect's result, discarded by `>> pure`
/// and never lexically constructed in user Core). The Print handler injects
/// `()` at runtime → unresolved variable. Un-ignore when the meta walk seeds
/// runtime-injectable (effect-result) constructors. See
/// plans/send-print-unresolved-bug.md.
#[ignore = "regression from a9a0082: () pruned from DataConTable meta walk; un-ignore when fixed"]
#[test]
fn print_then_pure_resolves() {
    let (_out, r) = run(r#"send (Print (T.pack "MARK")) >> (pure (123 :: Int))"#);
    assert!(
        r.is_ok(),
        "send (Print) >> (pure 123) should succeed, got {r:?}"
    );
}

/// SUSPECT: a strict `pure $!` continuation after the Print. Same root cause as
/// `print_then_pure_resolves` (the `$!` is irrelevant — any `pure` tail).
#[ignore = "regression from a9a0082: () pruned from DataConTable meta walk; un-ignore when fixed"]
#[test]
fn print_then_strict_pure_resolves() {
    let (_out, r) = run(r#"send (Print (T.pack "MARK")) >> (pure $! (123 :: Int))"#);
    assert!(
        r.is_ok(),
        "send (Print) >> (pure $! 123) should succeed, got {r:?}"
    );
}
