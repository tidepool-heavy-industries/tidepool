//! Regression coverage for #312: `T.replace` / `T.breakOn` returning null pointer
//! ("yield error: null pointer in effect result") on multi-line / composite-return
//! inputs through the JIT effect-dispatch path.
//!
//! These mirror the failing patterns from the original bug report (filed by
//! `pattern` agent runtime). They all pass on tidepool main as of this commit;
//! `pattern` was likely on a tidepool pinned before #309 (Data.Text.empty
//! intercept) or other post-#272 normalization fixes.
//!
//! Sister suite in `tidepool-runtime/tests/text_breakon_replace_pure.rs` covers
//! the pure JIT path.

use serde_json::json;
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
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

struct MockDispatcher;

impl DispatchEffect<()> for MockDispatcher {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        Err(tidepool_effect::error::EffectError::UnhandledEffect { tag })
    }
}

fn run_mcp(code: &str) -> serde_json::Value {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, None);

    let pp = prelude_dir();
    let ulp = user_lib_dir();
    assert!(
        ulp.join("Library.hs").exists(),
        ".tidepool/lib/Library.hs not found"
    );
    let include = [pp, ulp];

    let mut dispatcher = MockDispatcher;
    compile_and_run(&source, "result", &include, &mut dispatcher, &())
        .expect("compile_and_run failed")
        .to_json()
}

#[test]
fn replace_single_line() {
    assert_eq!(
        run_mcp(r#"pure (T.replace "world" "there" "hello world")"#),
        json!("hello there"),
    );
}

#[test]
fn replace_multiline_inline_newlines() {
    assert_eq!(
        run_mcp(r#"pure (T.replace "target" "X" "line one\nline two target here\nline three")"#),
        json!("line one\nline two X here\nline three"),
    );
}

#[test]
fn replace_with_unlines_body() {
    assert_eq!(
        run_mcp(
            r#"pure (T.replace "target" "REPLACED" (T.unlines [T.pack "line one", T.pack "line two target here", T.pack "line three"]))"#
        ),
        json!("line one\nline two REPLACED here\nline three\n"),
    );
}

#[test]
fn breakon_short_needle() {
    assert_eq!(
        run_mcp(r#"pure (T.breakOn "lo" "hello world")"#),
        json!(["hel", "lo world"]),
    );
}

#[test]
fn breakon_no_match() {
    assert_eq!(
        run_mcp(r#"pure (T.breakOn "NOPE" "hello world")"#),
        json!(["hello world", ""]),
    );
}

#[test]
fn breakon_match_at_zero() {
    assert_eq!(
        run_mcp(r#"pure (T.breakOn "hello" "hello world")"#),
        json!(["", "hello world"]),
    );
}

#[test]
fn breakon_multiline_returns_tuple() {
    assert_eq!(
        run_mcp(r#"pure (T.breakOn "target" "abc\ntarget\nxyz")"#),
        json!(["abc\n", "target\nxyz"]),
    );
}

/// Literal `let body = T.unlines ... in T.replace ...` shape from #312.
#[test]
fn issue_312_replace_literal_shape() {
    assert_eq!(
        run_mcp(
            r#"pure (let body = T.unlines [T.pack "line one", T.pack "line two with target here", T.pack "line three"]
                       in T.replace (T.pack "target") (T.pack "REPLACED") body)"#
        ),
        json!("line one\nline two with REPLACED here\nline three\n"),
    );
}

/// Literal `let (a, b) = T.breakOn ... in (T.length a, T.length b)` shape from #312.
#[test]
fn issue_312_breakon_length_pair() {
    assert_eq!(
        run_mcp(
            r#"pure (let { body = T.unlines [T.pack "line one", T.pack "line two with target here", T.pack "line three"]
                            ; (a, b) = T.breakOn (T.pack "target") body
                            } in (T.length a, T.length b))"#
        ),
        json!([23, 23]),
    );
}

fn long_body() -> String {
    // ~4 kB markdown with `target` in the middle — matches the reporter's
    // "mid-kB markdown blocks" description.
    let mut s = String::new();
    for i in 0..30 {
        s.push_str(&format!(
            "## section {}\\n\\nthis is line {} of filler. ",
            i, i
        ));
        s.push_str("lorem ipsum dolor sit amet, consectetur adipiscing elit. ");
        s.push_str("sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\\n\\n");
        if i == 15 {
            s.push_str("HERE IS THE target IN THE MIDDLE.\\n\\n");
        }
    }
    s
}

#[test]
fn replace_long_body() {
    let body = long_body();
    let code =
        format!(r#"pure (T.replace (T.pack "target") (T.pack "REPLACED") (T.pack "{body}"))"#);
    let s = run_mcp(&code);
    let s = s.as_str().expect("expected string result");
    assert!(
        s.contains("REPLACED"),
        "REPLACED not found in long body output"
    );
    assert!(!s.contains("target"), "target should have been replaced");
}

#[test]
fn breakon_long_body() {
    let body = long_body();
    let code = format!(
        r#"let body = T.pack "{body}" in pure (let (a, b) = T.breakOn (T.pack "target") body in (T.length a, T.length b))"#
    );
    assert_eq!(run_mcp(&code), json!([2664, 2361]));
}
