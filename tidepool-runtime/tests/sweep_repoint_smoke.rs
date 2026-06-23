//! Smoke gate for the home-module Data.Text import sweep (every `import
//! qualified Data.Text as T` in haskell/lib repointed to the vendored
//! `Tidepool.Data.Text`).
//!
//! - `dedent_runtime_fix`: `Tidepool.TextFormat.dedent` uses
//!   `countLeading = T.length . T.takeWhile (== ' ')` — a section predicate
//!   through a home binding. With EXTERNAL `T` this was silently wrong (takeWhile
//!   returned the input unmodified → countLeading = full line length → dedent
//!   over-dropped). With `T` now vendored it is correct. This is a REAL runtime
//!   bug fix.
//! - `uri_quoter_accept` / `uri_quoter_reject`: the `[uri|…|]` validator
//!   (`Tidepool.QQ.Validate`, repointed) runs at SPLICE time (GHC-compiled, never
//!   JIT-affected) — these confirm the repoint didn't disturb the always-on QQ
//!   validator path. Accept compiles+runs; reject fails to compile.

use serde_json::{json, Value};
use std::path::Path;

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

/// Compile + run a pure `result = <body>` with the given pragmas/imports.
///
/// Runs on a 64MB-stack thread: importing the utils (`Tidepool.TextFormat`) AND the
/// vendored `Tidepool.Data.Text` deepens in-session emit recursion past the 2MB
/// default test-thread stack (the known compile-time emit-depth class — cf.
/// `repro_qq_union`'s 64MB wrapper; the live MCP server compiles on a 256MB
/// eval thread, `tidepool-mcp/src/lib.rs`, so this is a test-harness accommodation).
fn run_src(pragmas: &str, imports: &str, body: &str) -> Result<Value, String> {
    let pragmas = pragmas.to_string();
    let imports = imports.to_string();
    let body = body.to_string();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let src = format!(
                r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures{pragmas} #-}}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as DT
{imports}
default (Int, Text)

result :: _
result = {body}
"#
            );
            let pp = prelude_path();
            let include = [pp.as_path()];
            tidepool_runtime::compile_and_run_pure(&src, "result", &include)
                .map(|v| v.to_json())
                .map_err(|e| e.to_string())
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn dedent_runtime_fix() {
    // lines indented 2 and 4 → common indent 2 → drop 2 from each.
    // Pre-sweep (external T): countLeading returned full line length → wrong.
    let got = run_src(
        "",
        "import qualified Tidepool.TextFormat as TF",
        r#"TF.dedent (DT.pack "  hello\n    world")"#,
    )
    .expect("dedent compile/run");
    assert_eq!(got, json!("hello\n  world\n"));
}

#[test]
fn uri_quoter_accept() {
    let got = run_src(
        ", QuasiQuotes",
        "import Tidepool.QQ (uri)",
        r#"[uri|https://example.com/a/b?q=1|]"#,
    )
    .expect("valid [uri|…|] must compile");
    assert_eq!(got, json!("https://example.com/a/b?q=1"));
}

#[test]
fn uri_quoter_reject() {
    // Whitespace + no https scheme → uriCheck fails → splice fails → compile Err.
    let r = run_src(
        ", QuasiQuotes",
        "import Tidepool.QQ (uri)",
        r#"[uri|not a valid uri|]"#,
    );
    assert!(
        r.is_err(),
        "invalid [uri|…|] must be REJECTED at splice time, got Ok: {r:?}"
    );
}

#[test]
fn sg_quoter_accept() {
    // ast-grep metavar pattern with balanced parens — accepted by sgCheck.
    let got = run_src(
        ", QuasiQuotes",
        "import Tidepool.QQ (sg)",
        r#"[sg|fn $NAME($$$ARGS)|]"#,
    )
    .expect("valid [sg|…|] must compile");
    assert_eq!(got, json!("fn $NAME($$$ARGS)"));
}
