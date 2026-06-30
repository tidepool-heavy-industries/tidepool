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

use std::io;
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

    let output = cmd.output().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            SessionError::BinderExtraction(
                "tidepool-extract not found on PATH (set TIDEPOOL_EXTRACT)".to_string(),
            )
        } else {
            SessionError::Io(e)
        }
    })?;

    if !output.status.success() {
        return Err(SessionError::BinderExtraction(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
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
}
