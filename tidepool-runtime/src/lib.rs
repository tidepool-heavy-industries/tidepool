//! High-level runtime for compiling and executing Haskell source via Tidepool.
//!
//! Provides `compile_haskell` (source to Core) and `compile_and_run` (source to
//! evaluated result), with filesystem caching of compiled CBOR artifacts.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use thiserror::Error;
pub use tidepool_codegen::host_fns::{drain_diagnostics, push_diagnostic};
pub use tidepool_codegen::jit_machine::{CancelHandle, JitError, ResumeInput};
use tidepool_codegen::jit_machine::{JitEffectMachine, SuspendableOutcome};
pub use tidepool_effect::dispatch::DispatchEffect;
pub use tidepool_eval::value::Value;
use tidepool_repr::serial::{read_cbor, read_metadata, MetaWarnings, ReadError};
use tidepool_repr::{CoreExpr, DataConTable};

mod cache;
pub mod failclass;
pub mod paths;
mod render;
pub mod session;

pub use failclass::{
    classify, classify_compile, classify_session, FailureClass, FailureEnvelope, Phase,
};
pub use render::{value_to_json, EvalResult};

/// Result of successful Haskell compilation: a Core expression, DataCon metadata, and warnings.
#[derive(Debug)]
pub struct CompileResult {
    /// The compiled Core expression (the JIT/eval input).
    pub expr: CoreExpr,
    /// DataCon metadata the JIT needs to dispatch on constructors.
    pub table: DataConTable,
    /// Compile warnings (e.g. `has_io`, captured type).
    pub warnings: MetaWarnings,
}

/// Errors that can occur during Haskell compilation.
#[derive(Error, Debug)]
pub enum CompileError {
    /// I/O error during file operations or process execution.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// The `tidepool-extract` process failed (e.g., GHC parse/type error).
    #[error("Haskell compilation failed:\n{0}")]
    ExtractFailed(String),
    /// Failed to deserialize the CBOR output from `tidepool-extract`.
    #[error("CBOR deserialization error: {0}")]
    ReadError(#[from] ReadError),
    /// A required output file (.cbor or meta.cbor) was not produced by the extractor.
    #[error("Missing output file from extractor: {}", .0.display())]
    MissingOutput(PathBuf),
    /// The target binding has IO type, which is not supported.
    #[error("IO type detected in result binding. IO operations (unsafePerformIO, etc.) are not supported in the Tidepool sandbox.")]
    IOTypeDetected,
}

/// Unified error type for the compile-and-run pipeline.
#[derive(Error, Debug)]
pub enum RuntimeError {
    /// Error during Haskell compilation.
    #[error(transparent)]
    Compile(#[from] CompileError),
    /// Error during JIT execution.
    #[error(transparent)]
    Jit(#[from] JitError),
}

/// Extract the 1-based inclusive `(start, end)` line range of the user's own
/// submitted code from a generated module's `-- [user-lines] <start>:<end>`
/// marker (emitted by `tidepool_mcp::eval_prep::template_haskell_impl` on the
/// `__user` binding's closing-bracket line). Absent for sources that don't
/// carry the marker (e.g. session-lib declaration compiles) — callers must not
/// fabricate a range when this returns `None`.
fn extract_user_code_lines(source: &str) -> Option<(usize, usize)> {
    const NEEDLE: &str = "-- [user-lines] ";
    let pos = source.find(NEEDLE)?;
    let rest = &source[pos + NEEDLE.len()..];
    let range: &str = rest.lines().next()?;
    let (start_s, end_s) = range.split_once(':')?;
    let start = start_s.trim().parse::<usize>().ok()?;
    let end = end_s.trim().parse::<usize>().ok()?;
    Some((start, end))
}

/// Extract module name from Haskell source (e.g. "module Expr where" -> "Expr").
pub(crate) fn extract_module_name(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module ") {
            // "module Foo.Bar where" or "module Foo (" → take until whitespace/paren
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// Compiles Haskell source code to Tidepool Core at runtime.
///
/// This function shells out to `tidepool-extract` (which must be available on the system `$PATH`)
/// to perform GHC parsing, type-checking, and Core translation. It writes the source to a
/// temporary file, executes the extractor, and reads back the resulting CBOR and metadata.
///
/// Compiled results are cached in the XDG cache directory (typically `~/.cache/tidepool`)
/// to speed up repeated compilations. The cache key is derived from the source code,
/// the target binder, and a fingerprint of any included dependency directories.
///
/// # Arguments
/// * `source` - The Haskell source code to compile.
/// * `target` - The name of the top-level binder to use as the entry point (e.g., "main").
/// * `include` - Paths to directories containing Haskell modules to include in the search path.
///
/// # Returns
/// * `Ok((CoreExpr, DataConTable))` on success.
/// * `Err(CompileError)` if compilation fails, the extractor is missing, or output is invalid.
pub fn compile_haskell(
    source: &str,
    target: &str,
    include: &[&Path],
) -> Result<CompileResult, CompileError> {
    compile_haskell_salted(source, target, include, None)
}

/// As [`compile_haskell`], but mixes `cache_salt` into the cache key. The
/// declaration-accumulation lane ([`session::SessionLib`]) passes its
/// `(session, generation)` salt so per-session, per-generation compilations
/// never collide and a generation bump invalidates correctly. With `None` this
/// is byte-for-byte identical to [`compile_haskell`].
pub fn compile_haskell_salted(
    source: &str,
    target: &str,
    include: &[&Path],
    cache_salt: Option<&str>,
) -> Result<CompileResult, CompileError> {
    let key = cache::cache_key_salted(source, target, include, cache_salt);
    if let Some((expr_bytes, meta_bytes)) = cache::cache_load(&key) {
        // Attempt to deserialize cached data. If this fails, treat it as a cache
        // miss and fall through to recompilation instead of propagating the error.
        if let (Ok(expr), Ok((table, warnings))) =
            (read_cbor(&expr_bytes), read_metadata(&meta_bytes))
        {
            tidepool_codegen::host_fns::register_var_names(&warnings.var_names);
            return Ok(CompileResult {
                expr,
                table,
                warnings,
            });
        }
    }

    // 1. Setup temporary workspace
    // Derive filename from the module declaration so GHC's module name matches
    // the filename (GhcPipeline uses capitalize(takeBaseName(path)) as target).
    let temp_dir = TempDir::new()?;
    let filename =
        extract_module_name(source).map_or_else(|| "Input.hs".to_string(), |m| format!("{}.hs", m));
    let input_path = temp_dir.path().join(&filename);
    std::fs::write(&input_path, source)?;

    // 2. Execute tidepool-extract
    // Arguments: <file.hs> --output-dir <dir> --target <name> [--include <dir> ...]
    let extract_bin =
        std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());
    let mut cmd = Command::new(&extract_bin);
    cmd.arg(&input_path);
    cmd.arg("--output-dir").arg(temp_dir.path());
    cmd.arg("--target").arg(target);

    for path in include {
        cmd.arg("--include").arg(path);
    }

    if let Some((start, end)) = extract_user_code_lines(source) {
        cmd.arg("--user-code-lines").arg(format!("{start}:{end}"));
    }

    let output = cmd.output().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            io::Error::new(
                io::ErrorKind::NotFound,
                "tidepool-extract not found on PATH. Ensure the Tidepool harness is installed.",
            )
        } else {
            e
        }
    })?;

    // Always print stderr for diagnostics (trace output from Haskell)
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    if !stderr_str.is_empty() {
        eprintln!("[tidepool-extract stderr]\n{}", stderr_str);
    }

    if !output.status.success() {
        return Err(CompileError::ExtractFailed(stderr_str.into_owned()));
    }

    // 3. Read and deserialize outputs
    let expr_path = temp_dir.path().join(format!("{}.cbor", target));
    let meta_path = temp_dir.path().join("meta.cbor");

    if !expr_path.exists() {
        return Err(CompileError::MissingOutput(expr_path));
    }
    if !meta_path.exists() {
        return Err(CompileError::MissingOutput(meta_path));
    }

    let expr_bytes = std::fs::read(&expr_path)?;
    let meta_bytes = std::fs::read(&meta_path)?;

    let expr = read_cbor(&expr_bytes)?;
    let (table, warnings) = read_metadata(&meta_bytes)?;
    // Register varId → name pairs so runtime unresolved-variable errors can
    // name the symbol (friction #12); same on the cache-hit path above and the
    // session-turn reader (session/turn.rs).
    tidepool_codegen::host_fns::register_var_names(&warnings.var_names);

    // Only store in cache if deserialization succeeded
    cache::cache_store(&key, &expr_bytes, &meta_bytes);

    Ok(CompileResult {
        expr,
        table,
        warnings,
    })
}

/// Default JIT allocation nursery size (64 MiB), used by [`compile_and_run`]
/// and [`compile_and_run_pure`].
pub const DEFAULT_NURSERY_SIZE: usize = 1 << 26; // 64 MiB

/// Stack size for eval threads. The JIT's clean recursion-overflow guard needs
/// stack headroom; too small a stack lets a deep non-tail recursion blow the
/// host stack into corruption ("unexpected heap tag") before the guard fires.
/// Shared by the MCP server's eval thread and the test harness so they can't
/// drift — a smaller test stack made the overflow probes diverge from real evals.
pub const EVAL_STACK_SIZE: usize = 256 * 1024 * 1024; // 256 MiB

/// Compile Haskell source and run it with the given effect handlers,
/// using the specified nursery size.
///
/// # Arguments
/// * `source` - The Haskell source code to compile.
/// * `target` - The name of the entry point binder.
/// * `include` - Search paths for Haskell modules.
/// * `handlers` - Effect dispatchers for the JIT machine.
/// * `user` - User context for effect handlers.
/// * `nursery_size` - Size of the allocation nursery in bytes.
///
/// # Returns
/// * `Ok(EvalResult)` on successful execution.
/// * `Err(RuntimeError)` for compilation or JIT execution errors.
pub fn compile_and_run_with_nursery_size<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
    nursery_size: usize,
) -> Result<EvalResult, RuntimeError> {
    compile_and_run_cancellable(
        source,
        target,
        include,
        handlers,
        user,
        nursery_size,
        |_| {},
    )
}

/// As [`compile_and_run_with_nursery_size`], but hands the freshly-built machine's
/// [`CancelHandle`] to `on_ready` BEFORE the (blocking) run begins.
///
/// The handle is `Send + Sync + Clone`, so a caller running this on a worker
/// thread can ship a clone to a watchdog/timeout task that flips it; the running
/// program then aborts at its next GC/tail-call safepoint with
/// `YieldError::Cancelled`, freeing the thread (and any resources it pins). This
/// is how the eval/repl servers turn a turn timeout into an actual abort instead
/// of a permanently-parked thread.
pub fn compile_and_run_cancellable<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
    nursery_size: usize,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<EvalResult, RuntimeError> {
    let CompileResult {
        expr,
        mut table,
        warnings,
    } = compile_haskell(source, target, include)?;
    if warnings.has_io {
        return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
    }
    // Populate type-sibling groups from case branches so that get_companion
    // can disambiguate constructors sharing unqualified names (e.g. Bin/Tip
    // from Data.Map vs Data.Set).
    table.populate_siblings_from_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, nursery_size)?;
    on_ready(machine.cancel_handle());
    let value = machine.run(&table, handlers, user)?;
    Ok(EvalResult::new(value, table, warnings.warnings))
}

/// The outcome of driving a turn that may SUSPEND at the ask boundary (E2
/// threadless suspension). On [`SuspendableRun::Suspended`] the machine's heap
/// is retained (session machinery) and the whole `JitEffectMachine` — plus the
/// `DataConTable` — is handed back so the caller can stow it as data (no parked
/// thread) and resume it later, on any thread, via [`resume_suspended_turn`].
// The `Suspended` variant carries a whole `JitEffectMachine` by design (that IS
// the stowed value); this enum is constructed and destructured immediately at
// the eval-thread boundary, so the size asymmetry is inherent, not a leak.
#[allow(clippy::large_enum_variant)]
pub enum SuspendableRun {
    /// The turn ran to completion.
    Completed(EvalResult),
    /// The turn suspended at the ask boundary.
    Suspended {
        /// The stowed machine (heap retained; continuation held internally).
        machine: JitEffectMachine,
        /// The constructor table this turn compiled against (needed to extract
        /// the prompt/meta from `request` and to convert the answer on resume).
        table: DataConTable,
        /// The bridged `Ask` request value.
        request: tidepool_eval::value::Value,
    },
}

/// The outcome of resuming a stowed turn (see [`resume_suspended_turn`]).
// `Completed(EvalResult)` is the large variant; like `SuspendableRun` this is a
// transient boundary carrier, destructured immediately by the caller.
#[allow(clippy::large_enum_variant)]
pub enum ResumedRun {
    /// The turn ran to completion.
    Completed(EvalResult),
    /// The turn suspended again at a further ask boundary. The machine (borrowed
    /// `&mut` by the resume) holds the new continuation internally, ready for
    /// another [`resume_suspended_turn`].
    Suspended {
        request: tidepool_eval::value::Value,
    },
}

/// Compile `source` and drive it until it COMPLETES or SUSPENDS at `ask_tag`
/// (the `Ask` union tag). Sibling of [`compile_and_run_cancellable`] that, at an
/// ask boundary, hands the machine back as data instead of blocking a thread —
/// the substrate for E2 threadless session suspension. The machine is compiled
/// as a SESSION machine so its heap is retained across the suspension (the drive
/// itself is byte-identical to the one-shot path for a turn that never asks).
#[allow(clippy::too_many_arguments)]
pub fn compile_and_run_suspendable<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
    nursery_size: usize,
    ask_tag: u64,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<SuspendableRun, RuntimeError> {
    let CompileResult {
        expr,
        mut table,
        warnings,
    } = compile_haskell(source, target, include)?;
    if warnings.has_io {
        return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
    }
    table.populate_siblings_from_expr(&expr);
    let mut machine = JitEffectMachine::compile_session(&expr, &table, nursery_size)?;
    on_ready(machine.cancel_handle());
    match machine.run_suspendable(&table, handlers, user, ask_tag)? {
        SuspendableOutcome::Completed(value) => Ok(SuspendableRun::Completed(EvalResult::new(
            value,
            table,
            warnings.warnings,
        ))),
        SuspendableOutcome::Suspended { request } => Ok(SuspendableRun::Suspended {
            machine,
            table,
            request,
        }),
    }
}

/// Re-enter a stowed turn (from [`compile_and_run_suspendable`]) with the
/// answer or an abort, driving to the next suspension or completion. Runs on
/// ANY thread — the machine re-installs its per-thread reach and re-points GC
/// state at its retained heap (never a nursery reset). `on_ready` receives the
/// machine's cancel handle before the (blocking) resume begins, exactly as
/// [`compile_and_run_cancellable`] does, so a runaway resume can be aborted.
pub fn resume_suspended_turn<U, H: DispatchEffect<U>>(
    machine: &mut JitEffectMachine,
    table: &DataConTable,
    handlers: &mut H,
    user: &U,
    ask_tag: u64,
    input: ResumeInput,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<ResumedRun, RuntimeError> {
    on_ready(machine.cancel_handle());
    match machine.resume_suspended(table, handlers, user, ask_tag, input)? {
        SuspendableOutcome::Completed(value) => {
            // No recompile happens on resume (the JIT machine is reused as-is),
            // so there are no new warnings to report here — they were already
            // surfaced on the turn that produced this continuation.
            Ok(ResumedRun::Completed(EvalResult::new(
                value,
                table.clone(),
                Vec::new(),
            )))
        }
        SuspendableOutcome::Suspended { request } => Ok(ResumedRun::Suspended { request }),
    }
}

/// Compile Haskell source and run it as a pure (non-effectful) program.
///
/// Skips freer-simple effect dispatch — the result is converted directly
/// from the heap. Use this for programs that don't use an `Eff` wrapper.
pub fn compile_and_run_pure(
    source: &str,
    target: &str,
    include: &[&Path],
) -> Result<EvalResult, RuntimeError> {
    compile_and_run_pure_salted(source, target, include, None)
}

/// As [`compile_and_run_pure`], but threads a `(session, generation)` cache salt
/// (see [`compile_haskell_salted`]). The declaration-accumulation lane passes
/// [`session::SessionLib::cache_salt`] so per-session, per-generation compiles
/// of identical-text probes never collide and a generation bump invalidates.
pub fn compile_and_run_pure_salted(
    source: &str,
    target: &str,
    include: &[&Path],
    cache_salt: Option<&str>,
) -> Result<EvalResult, RuntimeError> {
    let CompileResult {
        expr,
        mut table,
        warnings,
    } = compile_haskell_salted(source, target, include, cache_salt)?;
    if warnings.has_io {
        return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
    }
    table.populate_siblings_from_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, DEFAULT_NURSERY_SIZE)?;
    let value = machine.run_pure()?;
    Ok(EvalResult::new(value, table, warnings.warnings))
}

/// Compile Haskell source and run it with the given effect handlers,
/// using the default nursery size (64 MiB).
///
/// # Arguments
/// * `source` - The Haskell source code to compile.
/// * `target` - The name of the entry point binder.
/// * `include` - Search paths for Haskell modules.
/// * `handlers` - Effect dispatchers for the JIT machine.
/// * `user` - User context for effect handlers.
///
/// # Returns
/// * `Ok(EvalResult)` on successful execution.
/// * `Err(RuntimeError)` for compilation or JIT execution errors.
pub fn compile_and_run<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
) -> Result<EvalResult, RuntimeError> {
    compile_and_run_with_nursery_size(
        source,
        target,
        include,
        handlers,
        user,
        DEFAULT_NURSERY_SIZE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Set up TIDEPOOL_EXTRACT env var and check GHC availability.
    /// Returns false if GHC is not available (test should skip).
    fn ensure_extract_available() -> bool {
        if std::env::var("TIDEPOOL_EXTRACT").is_err() {
            let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("haskell")
                .join("tidepool-extract");
            if bin.exists() {
                std::env::set_var("TIDEPOOL_EXTRACT", &bin);
            }
        }
        // GHC is needed by tidepool-extract; only available inside `nix develop`
        std::process::Command::new("ghc")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    #[serial]
    fn test_compile_identity() {
        if !ensure_extract_available() {
            eprintln!("Skipping: GHC not available (run inside `nix develop`)");
            return;
        }
        let source = "module Test where\nidentity x = x";
        let CompileResult { expr, .. } =
            compile_haskell(source, "identity", &[]).expect("Failed to compile identity");

        // identity = \x -> x — node count varies with GHC optimization level
        assert!(expr.nodes.len() >= 2);
    }

    /// H / Wave-0.3: the extractor captures the GHC-inferred type of the eval's
    /// top expression (the `__user` binding) and threads it out as
    /// `MetaWarnings::captured_type`. A module whose `__user` is `[1,2,3] :: [Int]`
    /// must report `[Int]` (GHC's `ppr` rendering of the list type).
    #[test]
    #[serial]
    fn test_captured_type_simple_list() {
        if !ensure_extract_available() {
            eprintln!("Skipping: GHC not available (run inside `nix develop`)");
            return;
        }
        let source = "module Probe where\n__user :: [Int]\n__user = [1, 2, 3]\n";
        let CompileResult { warnings, .. } =
            compile_haskell(source, "__user", &[]).expect("Failed to compile probe");
        eprintln!("captured_type = {:?}", warnings.captured_type);
        assert_eq!(warnings.captured_type.as_deref(), Some("[Int]"));
    }

    /// A binding with no explicit signature still gets a captured type; and an
    /// extraction with no `__user` binding reports `None` (fixture-style build).
    #[test]
    #[serial]
    fn test_captured_type_absent_without_user() {
        if !ensure_extract_available() {
            eprintln!("Skipping: GHC not available (run inside `nix develop`)");
            return;
        }
        let source = "module Probe where\nidentity x = x\n";
        let CompileResult { warnings, .. } =
            compile_haskell(source, "identity", &[]).expect("Failed to compile identity");
        assert_eq!(warnings.captured_type, None);
    }

    #[test]
    #[serial]
    fn test_compile_error() {
        if !ensure_extract_available() {
            eprintln!("Skipping: GHC not available (run inside `nix develop`)");
            return;
        }
        let source = "module Test where\nfoo = garbage";
        let res = compile_haskell(source, "foo", &[]);
        assert!(res.is_err());
        if let Err(CompileError::ExtractFailed(msg)) = res {
            assert!(
                msg.contains("Variable not in scope: garbage")
                    || msg.contains("not in scope: garbage")
            );
        } else {
            panic!("Expected ExtractFailed error, got {:?}", res);
        }
    }

    /// A compile that SUCCEEDS but triggers a GHC diagnostic (overlapping
    /// patterns — on by default, no -Wall needed) surfaces that warning in
    /// `MetaWarnings::warnings` instead of dropping it silently.
    #[test]
    #[serial]
    fn test_compile_warnings_captured() {
        if !ensure_extract_available() {
            eprintln!("Skipping: GHC not available (run inside `nix develop`)");
            return;
        }
        let source = "module WarnProbe where\n\
                       f :: Int -> Int\n\
                       f x = 1\n\
                       f x = 2\n\
                       \n\
                       result :: Int\n\
                       result = f 0\n";
        let CompileResult { warnings, .. } =
            compile_haskell(source, "result", &[]).expect("Failed to compile result");
        assert!(
            !warnings.warnings.is_empty(),
            "expected at least one GHC warning for the overlapping `f` clauses"
        );
        assert!(
            warnings
                .warnings
                .iter()
                .any(|w| w.to_lowercase().contains("overlapping")),
            "expected an overlapping-patterns warning, got: {:?}",
            warnings.warnings
        );
    }

    /// A clean compile (no diagnostics) reports no warnings.
    #[test]
    #[serial]
    fn test_compile_no_warnings_on_clean_source() {
        if !ensure_extract_available() {
            eprintln!("Skipping: GHC not available (run inside `nix develop`)");
            return;
        }
        let source = "module CleanProbe where\nresult :: Int\nresult = 1 + 1\n";
        let CompileResult { warnings, .. } =
            compile_haskell(source, "result", &[]).expect("Failed to compile result");
        assert!(
            warnings.warnings.is_empty(),
            "expected no warnings for a clean compile, got: {:?}",
            warnings.warnings
        );
    }
}
