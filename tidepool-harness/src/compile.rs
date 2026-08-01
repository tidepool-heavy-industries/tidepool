//! Turn compilation: Haskell source → `(CoreExpr, DataConTable, AsksSidecar)`.
//!
//! The turn engine compiles each model-written eval block the same way the
//! MCP server does (`template_haskell` wrapping + `tidepool-extract`), but the
//! harness ALSO needs the `asks.json` sidecar (#R0 typed-yield pass) that maps
//! a `returnControl`/`returnControlFork` site id to the rendered answer type.
//! `tidepool_runtime::compile_haskell` drops the extract tempdir before it
//! returns, so this module invokes `tidepool-extract` directly into a
//! kept-alive tempdir (mirroring `compile_haskell`'s own `Command`
//! construction) and reads all three outputs — `<target>.cbor`, `meta.cbor`,
//! `asks.json` — from it.
//!
//! This is deliberately NOT a fork of the runtime's caching compile: turns are
//! one-shot `M a` expressions, the sidecar is small, and the extract call is
//! the ~2s floor either way. Keeping it here means the harness owns the
//! sidecar contract end-to-end.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use tidepool_repr::serial::{read_cbor, read_metadata};
use tidepool_repr::{CoreExpr, DataConTable};

/// One `asks.json` entry: a yield-site id and its rendered answer type.
#[derive(Debug, Clone, Deserialize)]
pub struct AskSite {
    pub site: u32,
    #[serde(rename = "type")]
    pub ty: String,
}

/// The `asks.json` sidecar as a site-id → rendered-type map. Empty when the
/// source has no `returnControl`/`returnControlFork` sites (the extract always
/// writes the file — loud absence beats a silent missing lookup).
#[derive(Debug, Clone, Default)]
pub struct AsksSidecar {
    by_site: HashMap<u32, String>,
}

impl AsksSidecar {
    /// Build a sidecar from `(site, type)` pairs — the shape
    /// `tidepool_runtime`'s `compile_session_turn` returns (the harness reuses
    /// that session-aware compile for value-plane bind turns).
    pub fn from_pairs(pairs: Vec<(u32, String)>) -> Self {
        AsksSidecar {
            by_site: pairs.into_iter().collect(),
        }
    }

    /// The rendered answer type for a yield-site id, if the site is known.
    pub fn type_of(&self, site: u32) -> Option<&str> {
        self.by_site.get(&site).map(String::as_str)
    }

    /// Number of recorded sites.
    pub fn len(&self) -> usize {
        self.by_site.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_site.is_empty()
    }
}

/// A compiled turn: the Core expression, its constructor table, and the
/// typed-yield sidecar.
pub struct CompiledTurn {
    pub expr: CoreExpr,
    pub table: DataConTable,
    pub asks: AsksSidecar,
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("failed to spawn tidepool-extract ({bin}): {source}")]
    Spawn { bin: String, source: std::io::Error },
    #[error("tidepool-extract failed:\n{0}")]
    Extract(String),
    #[error("io error during compile: {0}")]
    Io(#[from] std::io::Error),
    #[error("missing extract output: {0}")]
    MissingOutput(PathBuf),
    #[error("failed to deserialize extract output: {0}")]
    Deserialize(String),
    #[error("failed to parse asks.json: {0}")]
    Asks(String),
}

/// Compile `source` (a fully-templated Haskell module) with entry binder
/// `target`, searching `include` for modules, into `(expr, table, asks)`.
///
/// `extract_bin` is the `tidepool-extract` binary path (normally from
/// `TIDEPOOL_EXTRACT`); pass it explicitly so the harness resolves it once at
/// construction rather than re-reading the env per turn.
pub fn compile_turn(
    extract_bin: &str,
    source: &str,
    target: &str,
    include: &[PathBuf],
) -> Result<CompiledTurn, CompileError> {
    let temp_dir = tempfile::TempDir::new()?;
    // GHC derives the module name from the filename (capitalize(basename)); the
    // templated preamble declares `module Expr`, so the file must be `Expr.hs`.
    let module = extract_module_name(source).unwrap_or_else(|| "Expr".to_string());
    let input_path = temp_dir.path().join(format!("{module}.hs"));
    std::fs::write(&input_path, source)?;

    let mut cmd = Command::new(extract_bin);
    cmd.arg(&input_path);
    cmd.arg("--output-dir").arg(temp_dir.path());
    cmd.arg("--target").arg(target);
    for path in include {
        cmd.arg("--include").arg(path);
    }
    let output = cmd.output().map_err(|source| CompileError::Spawn {
        bin: extract_bin.to_string(),
        source,
    })?;
    if !output.status.success() {
        return Err(CompileError::Extract(format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )));
    }

    let expr_path = temp_dir.path().join(format!("{target}.cbor"));
    let meta_path = temp_dir.path().join("meta.cbor");
    let asks_path = temp_dir.path().join("asks.json");

    if !expr_path.exists() {
        return Err(CompileError::MissingOutput(expr_path));
    }
    if !meta_path.exists() {
        return Err(CompileError::MissingOutput(meta_path));
    }

    let expr_bytes = std::fs::read(&expr_path)?;
    let meta_bytes = std::fs::read(&meta_path)?;
    let expr = read_cbor(&expr_bytes).map_err(|e| CompileError::Deserialize(e.to_string()))?;
    let (table, warnings) =
        read_metadata(&meta_bytes).map_err(|e| CompileError::Deserialize(e.to_string()))?;
    // Register varId → name pairs so runtime unresolved-variable errors can name
    // the symbol (mirrors compile_haskell).
    tidepool_codegen::host_fns::register_var_names(&warnings.var_names);

    let asks = load_asks(&asks_path)?;

    Ok(CompiledTurn { expr, table, asks })
}

/// Read + parse the `asks.json` sidecar. A missing file yields an empty
/// sidecar (older extract without the pass) rather than an error; a present but
/// malformed file is a hard error (a real regression to surface).
fn load_asks(path: &Path) -> Result<AsksSidecar, CompileError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AsksSidecar::default()),
        Err(e) => return Err(CompileError::Io(e)),
    };
    let sites: Vec<AskSite> =
        serde_json::from_slice(&bytes).map_err(|e| CompileError::Asks(e.to_string()))?;
    Ok(AsksSidecar {
        by_site: sites.into_iter().map(|s| (s.site, s.ty)).collect(),
    })
}

/// Extract the module name from a `module <Name> where` header (GHC derives the
/// filename from it). Mirrors `tidepool_runtime`'s private helper.
fn extract_module_name(source: &str) -> Option<String> {
    for line in source.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("module ") {
            let name = rest.split_whitespace().next()?;
            return Some(name.to_string());
        }
    }
    None
}
