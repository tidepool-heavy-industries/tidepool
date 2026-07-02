//! Wave 3b — session-eval turn compilation (the bind/reference extract seam).
//!
//! A `session_eval` turn is classified by GHC's parser (parse-only) into a BIND
//! (`x <- action` / `let x = e`) or an EXPR (bare expression), then compiled
//! through the session-aware extract path with the live `Tidepool.Session.Val.G<g>`
//! ifaces injected. On a BIND turn the extract also writes the thin session iface
//! (under `session_root`) and emits the [`BoundBinder`] sidecar this module
//! parses. See `plans/wave3b-contract.md` §3–§5.
//!
//! These calls deliberately bypass the memo cache in [`crate::compile_haskell`]:
//! a session turn has on-disk side effects (the iface write) and depends on
//! mutable session state (the injected ifaces), so a cache hit would be wrong.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use tidepool_repr::serial::{read_cbor, read_metadata, MetaWarnings};
use tidepool_repr::{CoreExpr, DataConTable};

use crate::{extract_module_name, CompileError};

/// Strict-force tier of a bound value (mirrors the extract's `BoundBinder.tier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueTier {
    /// First-order data — `deep_force`d to NF then tenured.
    Tier0Data,
    /// A closure/PAP — tenured as-is (not forced).
    Tier1Closure,
}

/// One binder a BIND turn introduces — the extract's `BoundBinder` JSON record.
#[derive(Clone, Debug)]
pub struct BoundBinder {
    /// The user-facing name (`"x"`).
    pub name: String,
    /// The `0xFE`-tagged stable id minted by `Translate.stableVarId` (carried as
    /// a decimal string in JSON to avoid f64 precision loss).
    pub var_id: u64,
    /// `Tidepool.Session.Val.G<g>` — the module whose thin iface was written.
    pub module: String,
    /// Tier of the bound value.
    pub tier: ValueTier,
    /// `ppr` of the bound value's type, for `:t`.
    pub type_display: String,
}

/// Decl-vs-bind-vs-expr classification of a turn (GHC-sourced, parse-only).
///
/// GHC's parser is the single authority (both declaration and statement
/// contexts are tried; see `Tidepool.Binders.classifyTurn`). Exactly one of
/// `is_decl` / `is_bind` is true, or both false for a bare expression.
#[derive(Clone, Debug)]
pub struct TurnClassification {
    /// Whether the turn is a top-level declaration (`f x = e`, `f :: T`,
    /// `x = 5`, `(a,b) = p`). Mutually exclusive with `is_bind`.
    pub is_decl: bool,
    /// Whether the turn binds (`x <- e` / `let x = e`). False ⇒ a decl or a
    /// bare expr (disambiguated by `is_decl`).
    pub is_bind: bool,
    /// The bound/declared names (GHC-sourced). Empty for a bare expr.
    pub binders: Vec<String>,
}

/// The result of compiling one session-eval turn.
pub struct SessionTurnResult {
    /// JIT-able Core for the turn's `result` binding.
    pub expr: CoreExpr,
    /// This turn's DataCon metadata (the repl merges it into the session table).
    pub table: DataConTable,
    /// Compile warnings (e.g. `has_io`).
    pub warnings: MetaWarnings,
    /// The binder(s) this turn introduced — non-empty only on a BIND turn.
    pub binders: Vec<BoundBinder>,
}

/// Arguments for the bind half of a turn (omit for an EXPR turn).
#[derive(Clone, Debug)]
pub struct SessionBind<'a> {
    /// The bound names (GHC-sourced, from [`classify_turn`]). One name for a
    /// single-binder turn; N names for a flat-tuple multi-binder turn.
    pub names: &'a [String],
    /// The generation of the `Val.G<g>` module to mint (shared by all N names).
    pub gen: u64,
}

fn extract_bin() -> String {
    std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string())
}

fn map_notfound(e: std::io::Error) -> CompileError {
    if e.kind() == std::io::ErrorKind::NotFound {
        CompileError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "tidepool-extract not found on PATH (set TIDEPOOL_EXTRACT).",
        ))
    } else {
        CompileError::Io(e)
    }
}

/// Classify a raw turn (`x <- e` / `let x = e` / a bare expression) via the
/// extract's parse-only `--emit-stmt-binders`. The binder name(s) come from
/// GHC's parser, never a Rust scanner (plan §5.0 / domain §6 R5).
pub fn classify_turn(turn_text: &str) -> Result<TurnClassification, CompileError> {
    let temp = TempDir::new()?;
    let src = temp.path().join("turn.hs");
    std::fs::write(&src, turn_text)?;
    let out = temp.path().join("stmt.json");

    let output = Command::new(extract_bin())
        .arg(&src)
        .arg("--emit-stmt-binders")
        .arg(&out)
        .output()
        .map_err(map_notfound)?;
    if !output.status.success() {
        return Err(CompileError::ExtractFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let json = std::fs::read_to_string(&out).map_err(CompileError::Io)?;
    parse_stmt_json(&json)
}

fn parse_stmt_json(json: &str) -> Result<TurnClassification, CompileError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CompileError::ExtractFailed(format!("invalid stmt-binder JSON: {e}")))?;
    let kind = v
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("expr");
    let binders = v
        .get("binders")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(TurnClassification {
        is_decl: kind == "decl",
        is_bind: kind == "bind",
        binders,
    })
}

/// Compile one session-eval turn through the session-aware extract path.
///
/// `wrapped_source` is the full wrapped module (target binder `result`).
/// `inject_modules` are the live `Tidepool.Session.Val.G<g'>` module names to
/// inject so the turn can reference earlier bindings. `session_root` is where
/// the `Val` ifaces are written/read. `bind` carries the new binder's name+gen
/// on a BIND turn (and triggers the thin-iface write + sidecar emission).
pub fn compile_session_turn(
    wrapped_source: &str,
    include: &[&Path],
    session_root: &Path,
    inject_modules: &[String],
    bind: Option<SessionBind<'_>>,
) -> Result<SessionTurnResult, CompileError> {
    let temp = TempDir::new()?;
    let filename = extract_module_name(wrapped_source)
        .map_or_else(|| "Input.hs".to_string(), |m| format!("{m}.hs"));
    let input = temp.path().join(&filename);
    std::fs::write(&input, wrapped_source)?;
    let bb_path = temp.path().join("bound_binders.json");

    let mut cmd = Command::new(extract_bin());
    cmd.arg(&input)
        .arg("--output-dir")
        .arg(temp.path())
        .arg("--target")
        .arg("result")
        .arg("--session-root")
        .arg(session_root);
    for m in inject_modules {
        cmd.arg("--inject-val").arg(m);
    }
    for p in include {
        cmd.arg("--include").arg(p);
    }
    let is_bind = bind.is_some();
    if let Some(ref b) = bind {
        cmd.arg("--session-bind")
            .arg("--bind-gen")
            .arg(b.gen.to_string())
            .arg("--emit-bound-binders")
            .arg(&bb_path);
        for name in b.names {
            cmd.arg("--bind-name").arg(name);
        }
    }

    let output = cmd.output().map_err(map_notfound)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        eprintln!("[tidepool-extract stderr]\n{stderr}");
    }
    if !output.status.success() {
        return Err(CompileError::ExtractFailed(stderr.into_owned()));
    }

    let expr_path = temp.path().join("result.cbor");
    let meta_path = temp.path().join("meta.cbor");
    if !expr_path.exists() {
        return Err(CompileError::MissingOutput(expr_path));
    }
    if !meta_path.exists() {
        return Err(CompileError::MissingOutput(meta_path));
    }
    let expr = read_cbor(&std::fs::read(&expr_path)?)?;
    let (table, warnings) = read_metadata(&std::fs::read(&meta_path)?)?;
    // Runtime unresolved-error naming (friction #12) — see lib.rs twin sites.
    tidepool_codegen::host_fns::register_var_names(&warnings.var_names);

    let binders = if is_bind {
        let json = std::fs::read_to_string(&bb_path).map_err(|e| {
            CompileError::ExtractFailed(format!("bind turn emitted no bound-binder sidecar: {e}"))
        })?;
        parse_bound_binders(&json)?
    } else {
        Vec::new()
    };

    Ok(SessionTurnResult {
        expr,
        table,
        warnings,
        binders,
    })
}

fn parse_bound_binders(json: &str) -> Result<Vec<BoundBinder>, CompileError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CompileError::ExtractFailed(format!("invalid bound-binder JSON: {e}")))?;
    let arr = v
        .get("binders")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| CompileError::ExtractFailed("bound-binder JSON missing `binders`".into()))?;
    arr.iter().map(parse_one_binder).collect()
}

fn parse_one_binder(v: &serde_json::Value) -> Result<BoundBinder, CompileError> {
    let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str);
    let name = s("name")
        .ok_or_else(|| CompileError::ExtractFailed("binder missing `name`".into()))?
        .to_string();
    // varId is a DECIMAL STRING (JSON f64 would truncate a 64-bit id).
    let var_id = s("varId")
        .ok_or_else(|| CompileError::ExtractFailed("binder missing `varId`".into()))?
        .parse::<u64>()
        .map_err(|e| CompileError::ExtractFailed(format!("binder varId not a u64: {e}")))?;
    let module = s("module")
        .ok_or_else(|| CompileError::ExtractFailed("binder missing `module`".into()))?
        .to_string();
    let tier = match s("tier") {
        Some("Tier1Closure") => ValueTier::Tier1Closure,
        Some("Tier0Data") | None => ValueTier::Tier0Data,
        Some(other) => {
            return Err(CompileError::ExtractFailed(format!(
                "unknown binder tier {other:?}"
            )))
        }
    };
    let type_display = s("typeDisplay").unwrap_or("").to_string();
    Ok(BoundBinder {
        name,
        var_id,
        module,
        tier,
        type_display,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bind_classification() {
        let c = parse_stmt_json(r#"{"kind":"bind","binders":["x"]}"#).unwrap();
        assert!(c.is_bind);
        assert!(!c.is_decl);
        assert_eq!(c.binders, vec!["x".to_string()]);
    }

    #[test]
    fn parses_expr_classification() {
        let c = parse_stmt_json(r#"{"kind":"expr","binders":[]}"#).unwrap();
        assert!(!c.is_bind);
        assert!(!c.is_decl);
        assert!(c.binders.is_empty());
    }

    #[test]
    fn parses_decl_classification() {
        let c = parse_stmt_json(r#"{"kind":"decl","binders":["sq"]}"#).unwrap();
        assert!(c.is_decl);
        assert!(!c.is_bind);
        assert_eq!(c.binders, vec!["sq".to_string()]);
    }

    #[test]
    fn parses_bound_binder_with_string_varid() {
        let raw = (0xFEu64 << 56) | 0x123456;
        let json = format!(
            r#"{{"binders":[{{"name":"x","varId":"{raw}","module":"Tidepool.Session.Val.G3","tier":"Tier0Data","typeDisplay":"Int"}}]}}"#
        );
        let bs = parse_bound_binders(&json).unwrap();
        assert_eq!(bs.len(), 1);
        assert_eq!(bs[0].name, "x");
        assert_eq!(bs[0].var_id, raw);
        assert_eq!(bs[0].tier, ValueTier::Tier0Data);
        assert_eq!(bs[0].module, "Tidepool.Session.Val.G3");
    }

    #[test]
    fn parses_tier1_closure() {
        let json = r#"{"binders":[{"name":"f","varId":"42","module":"Tidepool.Session.Val.G1","tier":"Tier1Closure","typeDisplay":"Int -> Int"}]}"#;
        let bs = parse_bound_binders(json).unwrap();
        assert_eq!(bs[0].tier, ValueTier::Tier1Closure);
    }
}
