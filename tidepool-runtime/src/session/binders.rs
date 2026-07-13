//! GHC-sourced binder extraction (plan §5.0, domain §6 R5).
//!
//! Binder names — needed to know which names a declaration turn (re)defines for
//! the selective re-export — come from **GHC**, never a Rust-side Haskell parser.
//! This module shells out to the `tidepool-extract` binary's `--emit-binders`
//! mode, which parses the declaration with GHC's own parser and emits the
//! introduced binders as structured JSON.
//!
//! Boundary contract (the extractor's JSON, written to `--emit-binders <out>`):
//! ```json
//! {"items":[{"kind":"value","name":"slug"},
//!           {"kind":"type","name":"Foo","cons":["A","B"]}]}
//! ```

use std::path::Path;
use std::process::Command;

use super::render::ExportItem;
use super::SessionError;

/// Wrap raw declaration text into a parseable module. The binder extractor only
/// *parses* (it does not typecheck or rename), so no imports are needed — a
/// qualified reference like `T.toLower` parses fine without `import qualified … as
/// T`. The pragma block matches the eval surface so GADT/where syntax etc. parses.
fn wrap_decls(decl_text: &str) -> String {
    format!(
        "{{-# LANGUAGE GADTs, OverloadedStrings, TypeOperators, DataKinds, \
         ScopedTypeVariables, BangPatterns, ViewPatterns, TupleSections, \
         MultiWayIf, LambdaCase, RecordWildCards, NamedFieldPuns, \
         DeriveFunctor, DeriveFoldable, DeriveTraversable, TypeApplications #-}}\n\
         module SessionDecls where\n{decl_text}\n"
    )
}

/// Extract the export items a declaration introduces, via GHC (parse-only).
pub fn extract_binders(
    decl_text: &str,
    include: &[&Path],
) -> Result<Vec<ExportItem>, SessionError> {
    let temp_dir = tempfile::TempDir::new()?;
    let input_path = temp_dir.path().join("SessionDecls.hs");
    let out_path = temp_dir.path().join("binders.json");
    std::fs::write(&input_path, wrap_decls(decl_text))?;

    let extract_bin =
        std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());
    let mut cmd = Command::new(&extract_bin);
    cmd.arg(&input_path);
    cmd.arg("--emit-binders").arg(&out_path);
    for path in include {
        cmd.arg("--include").arg(path);
    }

    // Spawn failure is an environment problem (`Io` → Infra), never
    // `BinderExtraction` (which classifies as the user's Haskell).
    let output = cmd
        .output()
        .map_err(|e| SessionError::Io(crate::extract_spawn_error(e)))?;

    if !output.status.success() {
        let text = match crate::diag::parse_diag_report(&output.stdout, &output.stderr) {
            Ok(report) => report
                .diagnostics
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
            Err(msg) => msg,
        };
        return Err(SessionError::BinderExtraction(text));
    }

    let json_text = std::fs::read_to_string(&out_path).map_err(|e| {
        SessionError::BinderExtraction(format!("extractor produced no binder output: {e}"))
    })?;
    parse_binders_json(&json_text)
}

/// Parse the extractor's `{"items":[...]}` JSON into export items.
pub(crate) fn parse_binders_json(json_text: &str) -> Result<Vec<ExportItem>, SessionError> {
    let v: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|e| SessionError::BinderExtraction(format!("invalid binder JSON: {e}")))?;
    let items = v
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            SessionError::BinderExtraction("binder JSON missing `items` array".into())
        })?;
    Ok(items.iter().filter_map(ExportItem::from_json).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_items() {
        let items = parse_binders_json(
            r#"{"items":[{"kind":"value","name":"slug"},
                        {"kind":"type","name":"Foo","cons":["A","B"]}]}"#,
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].head_name(), "slug");
        assert_eq!(items[1].render_entry(), "Foo(..)");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_binders_json("not json").is_err());
        assert!(parse_binders_json(r#"{"nope":1}"#).is_err());
    }

    /// A missing extractor binary is an environment problem: `SessionError::Io`
    /// (→ Infra), not `BinderExtraction` (→ UserHaskell). Safe to mutate the
    /// env var: nextest runs each test in its own process.
    #[test]
    fn missing_extractor_is_io_not_binder_extraction() {
        std::env::set_var("TIDEPOOL_EXTRACT", "/nonexistent/tidepool-extract-test");
        let err = extract_binders("x = 1", &[]).unwrap_err();
        assert!(
            matches!(&err, SessionError::Io(e) if e.kind() == std::io::ErrorKind::NotFound),
            "expected Io(NotFound), got {err:?}"
        );
    }
}
