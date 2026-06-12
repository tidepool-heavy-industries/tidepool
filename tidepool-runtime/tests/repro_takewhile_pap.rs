//! Regression coverage for gotcha-audit #14: `T.takeWhile` / `T.dropWhile`
//! partial-application (PAP) silent corruption — the last standing
//! silent-failure class.
//!
//! Historical bug (CLAUDE.md "Known Limits", gotcha-audit #14): the SATURATED
//! calls `T.takeWhile p t` / `T.dropWhile p t` were correct, but PARTIALLY
//! APPLIED forms — `map (T.takeWhile p) ts`, a named PAP `tw = T.takeWhile p`,
//! eta wrappers, and `(g . T.takeWhile p)` compositions — silently returned the
//! inputs UNMODIFIED (takeWhile) or empties (dropWhile). Wrong data, no crash.
//! The Prelude shadows `takeWhileT` / `dropWhileT` were the workaround.
//!
//! VERDICT (2026-06-11): the bug is DEAD. It was fixed in passing by the EPS
//! unpoison (commit 9a827a3) — interfaces now load unfoldings, so GHC
//! specialization fires and the Data.Text stream-fusion worker is produced
//! identically under PAP and saturation. This suite pins that fix: every shape
//! below — saturated control, section PAP, named PAP, eta, composition, both
//! an equality predicate and a range-compare predicate — must agree with GHC
//! semantics. Any silent regression flips these red.
//!
//! Pure JIT path via `compile_and_run_pure` (sister to
//! `text_breakon_replace_pure.rs`).

use serde_json::json;
use std::path::Path;

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

/// Compile + run a pure `result = <body>` binding and return its JSON.
/// `T.takeWhile` / `T.dropWhile` here are the REAL Data.Text functions
/// (via `import qualified Data.Text as T`), NOT the `takeWhileT` shadows —
/// this is the PAP path the gotcha implicated.
fn run(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
default (Int, Text)

result :: _
result = {body}
"#
    );
    let pp = prelude_path();
    let include = [pp.as_path()];
    tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure failed")
        .to_json()
}

// Slash-delimited inputs, exercised with the equality predicate `(/= '/')`.
const SLASH_INPUTS: &str = r#"[T.pack "hello/world", T.pack "foo/bar", T.pack "noSlash"]"#;
// Digit-prefixed inputs, exercised with the range predicate `isDig`.
const DIGIT_INPUTS: &str = r#"[T.pack "123abc", T.pack "42", T.pack "abc", T.pack "9z9"]"#;

// ---------------------------------------------------------------------------
// takeWhile
// ---------------------------------------------------------------------------

#[test]
fn takewhile_saturated_control() {
    // Saturated was always correct; this is the baseline the PAP forms must match.
    assert_eq!(
        run(r#"T.takeWhile (/= '/') (T.pack "hello/world")"#),
        json!("hello")
    );
}

#[test]
fn takewhile_section_pap_via_map() {
    // `map (T.takeWhile p) ts` — the canonical PAP trigger from the gotcha.
    // Pre-fix this silently returned the inputs unmodified.
    assert_eq!(
        run(&format!(r#"map (T.takeWhile (/= '/')) {SLASH_INPUTS}"#)),
        json!(["hello", "foo", "noSlash"]),
    );
}

#[test]
fn takewhile_named_pap() {
    // A named partial application bound in a `let`, then mapped — the strongest
    // PAP shape (a true closure stored under a name).
    assert_eq!(
        run(&format!(
            r#"let tw = T.takeWhile (/= '/') in map tw {SLASH_INPUTS}"#
        )),
        json!(["hello", "foo", "noSlash"]),
    );
}

#[test]
fn takewhile_eta_wrapper() {
    assert_eq!(
        run(&format!(
            r#"map (\p -> T.takeWhile (/= '/') p) {SLASH_INPUTS}"#
        )),
        json!(["hello", "foo", "noSlash"]),
    );
}

#[test]
fn takewhile_composition() {
    // `(g . T.takeWhile p)` — composition keeps the PAP unsaturated until the
    // outer map drives it.
    assert_eq!(
        run(&format!(
            r#"map (T.toUpper . T.takeWhile (/= '/')) {SLASH_INPUTS}"#
        )),
        json!(["HELLO", "FOO", "NOSLASH"]),
    );
}

#[test]
fn takewhile_range_predicate_pap() {
    // Range-compare predicate (not a Data.Char classy fn), PAP'd via map.
    assert_eq!(
        run(&format!(
            r#"let isDig c = c >= '0' && c <= '9' in map (T.takeWhile isDig) {DIGIT_INPUTS}"#
        )),
        json!(["123", "42", "", "9"]),
    );
}

#[test]
fn takewhile_composition_into_length() {
    // Forces the result through `T.length` so a silently-wrong (un-truncated)
    // value would change the number, not just the string.
    assert_eq!(
        run(&format!(
            r#"let isDig c = c >= '0' && c <= '9' in map (T.length . T.takeWhile isDig) {DIGIT_INPUTS}"#
        )),
        json!([3, 2, 0, 1]),
    );
}

// ---------------------------------------------------------------------------
// dropWhile
// ---------------------------------------------------------------------------

#[test]
fn dropwhile_saturated_control() {
    assert_eq!(
        run(r#"T.dropWhile (/= '/') (T.pack "hello/world")"#),
        json!("/world")
    );
}

#[test]
fn dropwhile_section_pap_via_map() {
    // Pre-fix this silently returned empties.
    assert_eq!(
        run(&format!(r#"map (T.dropWhile (/= '/')) {SLASH_INPUTS}"#)),
        json!(["/world", "/bar", ""]),
    );
}

#[test]
fn dropwhile_named_pap() {
    assert_eq!(
        run(&format!(
            r#"let dw = T.dropWhile (/= '/') in map dw {SLASH_INPUTS}"#
        )),
        json!(["/world", "/bar", ""]),
    );
}

#[test]
fn dropwhile_eta_wrapper() {
    assert_eq!(
        run(&format!(
            r#"map (\p -> T.dropWhile (/= '/') p) {SLASH_INPUTS}"#
        )),
        json!(["/world", "/bar", ""]),
    );
}

#[test]
fn dropwhile_composition() {
    assert_eq!(
        run(&format!(
            r#"map (T.toUpper . T.dropWhile (/= '/')) {SLASH_INPUTS}"#
        )),
        json!(["/WORLD", "/BAR", ""]),
    );
}

#[test]
fn dropwhile_range_predicate_pap() {
    assert_eq!(
        run(&format!(
            r#"let isDig c = c >= '0' && c <= '9' in map (T.dropWhile isDig) {DIGIT_INPUTS}"#
        )),
        json!(["abc", "", "abc", "z9"]),
    );
}

#[test]
fn dropwhile_composition_into_length() {
    assert_eq!(
        run(&format!(
            r#"let isDig c = c >= '0' && c <= '9' in map (T.length . T.dropWhile isDig) {DIGIT_INPUTS}"#
        )),
        json!([3, 0, 3, 2]),
    );
}
