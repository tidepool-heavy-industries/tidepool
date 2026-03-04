//! High-level runtime for compiling and executing Haskell source via Tidepool.
//!
//! Provides `compile_haskell` (source to Core) and `compile_and_run` (source to
//! evaluated result), with filesystem caching of compiled CBOR artifacts.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
pub use tidepool_codegen::host_fns::{drain_diagnostics, push_diagnostic};
use tidepool_codegen::jit_machine::JitEffectMachine;
pub use tidepool_codegen::jit_machine::JitError;
pub use tidepool_effect::dispatch::DispatchEffect;
pub use tidepool_eval::value::Value;
use tidepool_repr::serial::{read_cbor, read_metadata, MetaWarnings, ReadError};
use tidepool_repr::{CoreExpr, DataConTable};

mod cache;
mod render;

pub use render::{value_to_json, EvalResult};

/// Result of successful Haskell compilation: a Core expression, DataCon metadata, and warnings.
pub type CompileResult = (CoreExpr, DataConTable, MetaWarnings);

/// Errors that can occur during Haskell compilation.
#[derive(Debug)]
pub enum CompileError {
    /// I/O error during file operations or process execution.
    Io(io::Error),
    /// The `tidepool-extract` process failed (e.g., GHC parse/type error).
    ExtractFailed(String),
    /// Failed to deserialize the CBOR output from `tidepool-extract`.
    ReadError(ReadError),
    /// A required output file (.cbor or meta.cbor) was not produced by the extractor.
    MissingOutput(PathBuf),
    /// The target binding has IO type, which is not supported.
    IOTypeDetected,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::Io(e) => write!(f, "I/O error: {}", e),
            CompileError::ExtractFailed(msg) => write!(f, "Haskell compilation failed:\n{}", msg),
            CompileError::ReadError(e) => write!(f, "CBOR deserialization error: {}", e),
            CompileError::MissingOutput(path) => {
                write!(f, "Missing output file from extractor: {}", path.display())
            }
            CompileError::IOTypeDetected => {
                write!(f, "IO type detected in result binding. IO operations (unsafePerformIO, etc.) are not supported in the Tidepool sandbox.")
            }
        }
    }
}

impl std::error::Error for CompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CompileError::Io(e) => Some(e),
            CompileError::ReadError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for CompileError {
    fn from(e: io::Error) -> Self {
        CompileError::Io(e)
    }
}

impl From<ReadError> for CompileError {
    fn from(e: ReadError) -> Self {
        CompileError::ReadError(e)
    }
}

/// Unified error type for compile + run pipeline.
#[derive(Debug)]
pub enum RuntimeError {
    Compile(CompileError),
    Jit(JitError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::Compile(e) => write!(f, "{}", e),
            RuntimeError::Jit(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RuntimeError::Compile(e) => Some(e),
            RuntimeError::Jit(e) => Some(e),
        }
    }
}

impl From<CompileError> for RuntimeError {
    fn from(e: CompileError) -> Self {
        Self::Compile(e)
    }
}

impl From<JitError> for RuntimeError {
    fn from(e: JitError) -> Self {
        Self::Jit(e)
    }
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
    let key = cache::cache_key(source, target, include);
    if let Some((expr_bytes, meta_bytes)) = cache::cache_load(&key) {
        // Attempt to deserialize cached data. If this fails, treat it as a cache
        // miss and fall through to recompilation instead of propagating the error.
        if let (Ok(expr), Ok((table, warnings))) =
            (read_cbor(&expr_bytes), read_metadata(&meta_bytes))
        {
            return Ok((expr, table, warnings));
        }
    }

    // 1. Setup temporary workspace
    let temp_dir = TempDir::new()?;
    let input_path = temp_dir.path().join("input.hs");
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

    // Only store in cache if deserialization succeeded
    cache::cache_store(&key, &expr_bytes, &meta_bytes);

    Ok((expr, table, warnings))
}

const DEFAULT_NURSERY_SIZE: usize = 1 << 26; // 64 MiB

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
    let (expr, mut table, warnings) = compile_haskell(source, target, include)?;
    if warnings.has_io {
        return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
    }
    // Populate type-sibling groups from case branches so that get_companion
    // can disambiguate constructors sharing unqualified names (e.g. Bin/Tip
    // from Data.Map vs Data.Set).
    table.populate_siblings_from_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, nursery_size)?;
    let value = machine.run(&table, handlers, user)?;
    Ok(EvalResult::new(value, table))
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
    let (expr, mut table, warnings) = compile_haskell(source, target, include)?;
    if warnings.has_io {
        return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
    }
    table.populate_siblings_from_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, DEFAULT_NURSERY_SIZE)?;
    let value = machine.run_pure()?;
    Ok(EvalResult::new(value, table))
}

/// Compile Haskell source and run it with the given effect handlers,
/// using the default nursery size (1 MiB).
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

    #[test]
    #[ignore] // Manual test: requires tidepool-extract on PATH
    fn test_compile_identity() {
        let source = "module Test where\nidentity x = x";
        let (expr, _table, _warnings) =
            compile_haskell(source, "identity", &[]).expect("Failed to compile identity");

        // identity = \x -> x, should have 2 nodes: [Var(x), Lam(x, 0)]
        assert_eq!(expr.nodes.len(), 2);
    }

    #[test]
    #[ignore] // Manual test: requires tidepool-extract on PATH
    fn test_compile_error() {
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
}
