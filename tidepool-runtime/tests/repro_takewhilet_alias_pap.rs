//! Sister matrix to `repro_takewhile_pap.rs`, exercising the `takeWhileT` /
//! `dropWhileT` Prelude SHADOWS (not the raw `Data.Text` functions) through
//! every PAP and predicate shape. This is the verification gate from
//! `plans/takewhile-shadow-retirement.md` — and it is the gate that REJECTED the
//! retirement.
//!
//! HISTORY: `takeWhileT` / `dropWhileT` are pure `T.pack . go . T.unpack`
//! reimplementations that began as a workaround for gotcha-audit #14 —
//! `T.takeWhile` / `T.dropWhile` were silently wrong under partial application.
//! That DIRECT bug is dead (EPS unpoison, commit 9a827a3); `repro_takewhile_pap.rs`
//! pins the fixed `Data.Text` PAP path. The plan proposed retiring these shadows
//! to thin delegations (`takeWhileT = T.takeWhile`, or eta-expanded
//! `takeWhileT p t = T.takeWhile p t`) on the theory they were now redundant.
//!
//! VERDICT (2026-06-11): the delegation is BROKEN, the shadows are LOAD-BEARING.
//! Measured as a three-way control through THIS identical harness — the manual
//! `T.pack . go . T.unpack` body goes 14/14 GREEN (the current/shipped state),
//! while the eta-reduced `takeWhileT = T.takeWhile` and the eta-expanded
//! `takeWhileT p t = T.takeWhile p t` delegations BOTH go 10/14 RED (the same 10
//! failures).
//!
//! The corruption fires whenever `T.takeWhile` is reached through the cross-module
//! Prelude wrapper with an operator-section predicate like `(/= '/')` — even a
//! fully saturated `takeWhileT (/= '/') t` returns the input unmodified — but NOT
//! with a named predicate (`isDig`). The unpoison fixed `T.takeWhile` used
//! DIRECTLY in a user module; it did not fix `T.takeWhile` wrapped behind a
//! Prelude binding (a distinct, narrower codegen bug awaiting a mechanism fix).
//!
//! So this suite PASSES against the manual shadows and stands as a tripwire: if
//! anyone re-attempts the delegation (or the underlying codegen bug regresses),
//! the `(/= '/')` cases flip red while `repro_takewhile_pap.rs` (the direct path)
//! stays green — exactly the gap that proves the shadows are still needed.
//!
//! Pure JIT path via `compile_and_run_pure`.

use serde_json::json;
use std::path::Path;

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

/// Compile + run a pure `result = <body>` binding and return its JSON.
/// `takeWhileT` / `dropWhileT` here are the Prelude SHADOWS (aliases for
/// `T.takeWhile` / `T.dropWhile`), in scope via `import Tidepool.Prelude`.
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
// takeWhileT
// ---------------------------------------------------------------------------

#[test]
fn takewhilet_saturated_control() {
    assert_eq!(
        run(r#"takeWhileT (/= '/') (T.pack "hello/world")"#),
        json!("hello")
    );
}

#[test]
fn takewhilet_section_pap_via_map() {
    // `map (takeWhileT p) ts` — the canonical PAP trigger, now through the alias.
    assert_eq!(
        run(&format!(r#"map (takeWhileT (/= '/')) {SLASH_INPUTS}"#)),
        json!(["hello", "foo", "noSlash"]),
    );
}

#[test]
fn takewhilet_named_pap() {
    // A named partial application bound in a `let`, then mapped — the strongest
    // PAP shape (a true closure stored under a name).
    assert_eq!(
        run(&format!(
            r#"let tw = takeWhileT (/= '/') in map tw {SLASH_INPUTS}"#
        )),
        json!(["hello", "foo", "noSlash"]),
    );
}

#[test]
fn takewhilet_eta_wrapper() {
    assert_eq!(
        run(&format!(
            r#"map (\p -> takeWhileT (/= '/') p) {SLASH_INPUTS}"#
        )),
        json!(["hello", "foo", "noSlash"]),
    );
}

#[test]
fn takewhilet_composition() {
    assert_eq!(
        run(&format!(
            r#"map (T.toUpper . takeWhileT (/= '/')) {SLASH_INPUTS}"#
        )),
        json!(["HELLO", "FOO", "NOSLASH"]),
    );
}

#[test]
fn takewhilet_range_predicate_pap() {
    assert_eq!(
        run(&format!(
            r#"let isDig c = c >= '0' && c <= '9' in map (takeWhileT isDig) {DIGIT_INPUTS}"#
        )),
        json!(["123", "42", "", "9"]),
    );
}

#[test]
fn takewhilet_composition_into_length() {
    // Forces the result through `T.length` so a silently-wrong (un-truncated)
    // value would change the number, not just the string.
    assert_eq!(
        run(&format!(
            r#"let isDig c = c >= '0' && c <= '9' in map (T.length . takeWhileT isDig) {DIGIT_INPUTS}"#
        )),
        json!([3, 2, 0, 1]),
    );
}

// ---------------------------------------------------------------------------
// dropWhileT
// ---------------------------------------------------------------------------

#[test]
fn dropwhilet_saturated_control() {
    assert_eq!(
        run(r#"dropWhileT (/= '/') (T.pack "hello/world")"#),
        json!("/world")
    );
}

#[test]
fn dropwhilet_section_pap_via_map() {
    assert_eq!(
        run(&format!(r#"map (dropWhileT (/= '/')) {SLASH_INPUTS}"#)),
        json!(["/world", "/bar", ""]),
    );
}

#[test]
fn dropwhilet_named_pap() {
    assert_eq!(
        run(&format!(
            r#"let dw = dropWhileT (/= '/') in map dw {SLASH_INPUTS}"#
        )),
        json!(["/world", "/bar", ""]),
    );
}

#[test]
fn dropwhilet_eta_wrapper() {
    assert_eq!(
        run(&format!(
            r#"map (\p -> dropWhileT (/= '/') p) {SLASH_INPUTS}"#
        )),
        json!(["/world", "/bar", ""]),
    );
}

#[test]
fn dropwhilet_composition() {
    assert_eq!(
        run(&format!(
            r#"map (T.toUpper . dropWhileT (/= '/')) {SLASH_INPUTS}"#
        )),
        json!(["/WORLD", "/BAR", ""]),
    );
}

#[test]
fn dropwhilet_range_predicate_pap() {
    assert_eq!(
        run(&format!(
            r#"let isDig c = c >= '0' && c <= '9' in map (dropWhileT isDig) {DIGIT_INPUTS}"#
        )),
        json!(["abc", "", "abc", "z9"]),
    );
}

#[test]
fn dropwhilet_composition_into_length() {
    assert_eq!(
        run(&format!(
            r#"let isDig c = c >= '0' && c <= '9' in map (T.length . dropWhileT isDig) {DIGIT_INPUTS}"#
        )),
        json!([3, 0, 3, 2]),
    );
}
