//! Regression coverage for the 2026-06 GC root-cause fixes and error-message
//! surfacing. History: a tail-strict accumulator filter over large/infinite
//! input raw-SIGSEGV'd the process. Root causes fixed:
//!
//! 1. `heap_force` held `current` in a host-frame local across thunk calls;
//!    the frame walker skips host frames and from-space is freed after every
//!    collection, so the local dangled. Fixed via scoped RUST_ROOTS
//!    registration (plus the same discipline in `apply_cont_heap`).
//! 2. The heap never grew: large live sets hit premature OOM after GC thrash.
//!    Fixed via a second Cheney pass into a doubled space at high utilization
//!    (cap: `TIDEPOOL_MAX_HEAP`, default 1 GiB).
//! 3. `compile_and_run_pure` never installed signal handlers, so any JIT fault
//!    killed the embedding process. Fixed: `install()` in run/run_pure.
//! 4. Error messages were dropped: extraction required a bare `LitString`
//!    argument (the Text `error` shadow wraps it), and ⊥ flowing to the result
//!    was misread by the bridge as data. Fixed: subtree literal scan +
//!    poison-aware bridge.

use std::path::Path;

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

fn run_capture(body_decls: &str) -> Result<serde_json::Value, String> {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
default (Int, Text)

{body_decls}
"#
    );
    let pp = prelude_path();
    let include = [pp.as_path()];
    tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .map(|v| v.to_json())
        .map_err(|e| format!("{e}"))
}

/// A tail-strict accumulator filter: every iteration allocates, the live set
/// grows monotonically, and the recursion is a trampolined tail call. This is
/// the shape that exercised all three GC bugs at once.
const STRICT_FILTER: &str = r#"
sfilter :: (Int -> Bool) -> [Int] -> [Int]
sfilter p = go []
  where
    go acc []     = reverse acc
    go acc (x:xs)
      | p x       = go (x : acc) xs
      | otherwise = go acc xs
"#;

#[test]
fn finite_large_accumulator_completes() {
    // 600K elements: live set crosses several heap doublings. Must return the
    // correct count — historically a process-killing SIGSEGV.
    let r = run_capture(&format!(
        "{STRICT_FILTER}\nresult :: Int\nresult = length (sfilter (\\x -> rem x 2 == 0) (enumFromTo 0 600000))\n"
    ));
    assert_eq!(r.ok(), Some(serde_json::json!(300001)));
}

#[test]
fn infinite_allocator_divergence_is_clean_error() {
    // Unbounded allocation must end in a clean heap-overflow yield error —
    // never a signal. TIDEPOOL_MAX_HEAP caps growth; the env var is read once
    // per process, so this test relies on the default cap unless the harness
    // sets it. Divergence to the cap is the cost of the assertion.
    let r = run_capture(&format!(
        "{STRICT_FILTER}\nnats :: Int -> [Int]\nnats n = n : nats (n + 1)\n\nresult :: [Int]\nresult = take 5 (sfilter (\\x -> rem x 2 == 0) (nats 0))\n"
    ));
    let err = r.expect_err("divergence must not succeed");
    assert!(
        err.contains("heap overflow"),
        "expected clean heap overflow, got: {err}"
    );
}

#[test]
fn prelude_filter_lazy_infinite() {
    // Guarded-corecursion filter (lazy-filter merge): take over an infinite
    // list terminates with correct results.
    let r = run_capture(
        r#"
nats :: Int -> [Int]
nats n = n : nats (n + 1)

result :: [Int]
result = take 5 (filter (\x -> rem x 2 == 0) (nats 0))
"#,
    );
    assert_eq!(r.ok(), Some(serde_json::json!([0, 2, 4, 6, 8])));
}

#[test]
fn error_message_surfaces() {
    // `error "msg"` must carry msg to the caller — through the Text shadow
    // (`error = P.error . T.unpack`), the lazy-poison binding path, and the
    // result bridge.
    let r = run_capture(
        r#"
result :: Int
result = if (3 :: Int) > 2 then error "test-message-123" else 0
"#,
    );
    let err = r.expect_err("error call must fail");
    assert!(err.contains("test-message-123"), "message dropped: {err}");
}

#[test]
fn error_message_surfaces_via_floated_binding() {
    // In larger modules GHC floats message literals to outer bindings, so the
    // error argument is a Var reference. Extraction must resolve through it.
    let r = run_capture(
        r#"
terror :: Text -> a
terror = error . unpack

sharedMsg :: Text
sharedMsg = "floated-message-77"

result :: Int
result = if (3 :: Int) > 2 then terror sharedMsg else 0
"#,
    );
    let err = r.expect_err("error call must fail");
    assert!(err.contains("floated-message-77"), "message dropped: {err}");
}
