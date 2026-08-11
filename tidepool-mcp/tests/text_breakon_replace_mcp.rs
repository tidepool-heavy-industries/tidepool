//! Regression coverage for #312: `T.replace` / `T.breakOn` returning null pointer
//! ("yield error: null pointer in effect result") on multi-line / composite-return
//! inputs through the JIT effect-dispatch path.
//!
//! Sister suite in `tidepool-runtime/tests/text_breakon_replace_pure.rs` covers
//! the pure JIT path.
//!
//! BUNDLED: named-check-list idiom (see
//! `tidepool-runtime/tests/generic_form_roundtrip.rs`'s `check` pattern) — ONE
//! Haskell compile per test, returning the list of FAILED check names, asserted
//! empty — same 11 behaviors, individually named in the failure output. Split
//! in two only because the long-body cases share one `T.pack`-compiled corpus
//! string the short, static cases don't need.

use serde_json::json;
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;
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
    let eff = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &std::path::Path;
    let include = [pp, ulp.as_path(), eff];

    let mut dispatcher = MockDispatcher;
    compile_and_run(&source, "result", &include, &mut dispatcher, &())
        .expect("compile_and_run failed")
        .to_json()
}

/// The 9 short, static-input cases (plain replace/breakOn shapes plus the two
/// literal #312 repro shapes) as one named-check-list eval: each `check`
/// entry is the case's original test name, so a failure names exactly which
/// behavior broke.
#[test]
fn breakon_replace_short_cases_all_pass() {
    let code = r#"pure (toJSON failed)
  where
    check :: Text -> Bool -> [Text]
    check nm ok = if ok then [] else [nm]

    failed :: [Text]
    failed = concat
      [ check "replace_single_line"
          (T.replace "world" "there" "hello world" == "hello there")
      , check "replace_multiline_inline_newlines"
          (T.replace "target" "X" "line one\nline two target here\nline three"
             == "line one\nline two X here\nline three")
      , check "replace_with_unlines_body"
          (T.replace "target" "REPLACED"
             (T.unlines ["line one", "line two target here", "line three"])
             == "line one\nline two REPLACED here\nline three\n")
      , check "breakon_short_needle"
          (T.breakOn "lo" "hello world" == ("hel", "lo world"))
      , check "breakon_no_match"
          (T.breakOn "NOPE" "hello world" == ("hello world", ""))
      , check "breakon_match_at_zero"
          (T.breakOn "hello" "hello world" == ("", "hello world"))
      , check "breakon_multiline_returns_tuple"
          (T.breakOn "target" "abc\ntarget\nxyz" == ("abc\n", "target\nxyz"))
      , check "issue_312_replace_literal_shape"
          (let body = T.unlines
                 ["line one", "line two with target here", "line three"]
           in T.replace "target" "REPLACED" body
                == "line one\nline two with REPLACED here\nline three\n")
      , check "issue_312_breakon_length_pair"
          (let { body = T.unlines
                   ["line one", "line two with target here", "line three"]
               ; (a, b) = T.breakOn "target" body
               }
           in (T.length a, T.length b) == (23, 23))
      ]
"#;
    assert_eq!(
        run_mcp(code),
        json!([]),
        "failed checks (named above) — see the original per-case test for the exact shape"
    );
}

/// ~4 kB markdown with `target` in the middle — matches the reporter's
/// "mid-kB markdown blocks" description. Shared verbatim by both cases below
/// so the corpus is compiled into the source once, not twice.
fn long_body() -> String {
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

/// The 2 long-body cases (replace and breakOn over the ~4kB corpus) as one
/// named-check-list eval, sharing a single `longBody` binding.
#[test]
fn breakon_replace_long_body_cases_all_pass() {
    let body = long_body();
    let code = format!(
        r#"pure (toJSON failed)
  where
    check :: Text -> Bool -> [Text]
    check nm ok = if ok then [] else [nm]

    longBody :: Text
    longBody = T.pack "{body}"

    failed :: [Text]
    failed = concat
      [ check "replace_long_body"
          (let out = T.replace "target" "REPLACED" longBody
           in T.isInfixOf "REPLACED" out && not (T.isInfixOf "target" out))
      , check "breakon_long_body"
          (let (a, b) = T.breakOn "target" longBody
           in (T.length a, T.length b) == (2664, 2361))
      ]
"#
    );
    assert_eq!(
        run_mcp(&code),
        json!([]),
        "failed checks (named above) — see the original per-case test for the exact shape"
    );
}
