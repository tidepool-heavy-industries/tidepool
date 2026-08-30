//! Pins the real failure mode of an IO-capable import in an eval, now that
//! the import blocklist (`rejected_import`) is gone: the JIT has no
//! FFI/IO lowering (no ForeignCall/CCall emission in tidepool-codegen), so
//! an errant `import System.IO.Unsafe` + `unsafePerformIO` cannot execute
//! anything — it fails as a structured compile diagnostic surfaced to the
//! caller, never a crash or a silent wrong answer. `getEnv` is real `IO`
//! (unshadowed by `Tidepool.Prelude`, unlike `readFile`), so this exercises
//! genuine IO-primop lowering rather than a type mismatch against the
//! effect-verb namespace.
//!
//! Compile-fail assertion — kept standalone per root CLAUDE.md's family-bundle
//! rule (bundling would let this failure destroy a sibling success-path
//! compile's diagnosis).

use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::{compile_and_run, CompileError, RuntimeError};
use tidepool_testing::eval_harness::user_lib_dir;

fn prelude_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell/lib")
        .leak()
}

struct MockDispatcher;

impl DispatchEffect<()> for MockDispatcher {
    fn dispatch(
        &mut self,
        _request: &Value,
        _cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<Option<tidepool_effect::Response>, tidepool_effect::error::EffectError> {
        Ok(None)
    }
}

#[test]
fn unsafe_io_import_fails_as_compile_diagnostic_not_a_crash() {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = "pure (unsafePerformIO (getEnv \"HOME\") :: String)";
    let imports = "System.IO.Unsafe (unsafePerformIO)\nSystem.Environment (getEnv)\n";
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, imports, "", None, None);

    let pp = prelude_dir();
    let ulp = user_lib_dir();
    let dirs = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let core = dirs.core.leak() as &Path;
    let shim = dirs.shim.leak() as &Path;
    let include = [pp, ulp.as_path(), core, shim];

    let mut dispatcher = MockDispatcher;
    let result = compile_and_run(&source, "result", &include, &mut dispatcher, &());

    match result {
        Ok(v) => panic!(
            "expected an IO-capable import to fail cleanly, got a successful result: {:?}",
            v.value()
        ),
        Err(RuntimeError::Compile(CompileError::Diagnostics(diags))) => {
            assert!(
                !diags.is_empty(),
                "Diagnostics variant must actually carry at least one diagnostic"
            );
        }
        Err(RuntimeError::Compile(CompileError::ExtractFailed(msg))) => {
            assert!(!msg.is_empty(), "ExtractFailed message must not be empty");
        }
        Err(other) => panic!(
            "expected a Compile(Diagnostics|ExtractFailed) error surfacing the unsupported \
             IO primop, got a different error shape: {other:?}"
        ),
    }
}
