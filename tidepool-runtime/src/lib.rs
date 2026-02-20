use core_repr::serial::{read_cbor, read_metadata, ReadError};
use core_repr::{CoreExpr, DataConTable};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Result of successful Haskell compilation: a Core expression and its associated DataCon metadata.
pub type CompileResult = (CoreExpr, DataConTable);

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

/// Compiles Haskell source code to Tidepool Core at runtime.
///
/// This function shells out to `tidepool-extract` (which must be available on the system `$PATH`)
/// to perform GHC parsing, type-checking, and Core translation. It writes the source to a 
/// temporary file, executes the extractor, and reads back the resulting CBOR and metadata.
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
    // 1. Setup temporary workspace
    let temp_dir = TempDir::new()?;
    let input_path = temp_dir.path().join("input.hs");
    std::fs::write(&input_path, source)?;

    // 2. Execute tidepool-extract
    // Arguments: <file.hs> --output-dir <dir> --target <name> [--include <dir> ...]
    let mut cmd = Command::new("tidepool-extract");
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

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(CompileError::ExtractFailed(stderr));
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
    let table = read_metadata(&meta_bytes)?;

    Ok((expr, table))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Manual test: requires tidepool-extract on PATH
    fn test_compile_identity() {
        let source = "module Test where\nidentity x = x";
        let (expr, _table) = compile_haskell(source, "identity", &[])
            .expect("Failed to compile identity");

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
            assert!(msg.contains("Variable not in scope: garbage") || msg.contains("not in scope: garbage"));
        } else {
            panic!("Expected ExtractFailed error, got {:?}", res);
        }
    }
}