//! `:i <Name>` stdlib/preamble resolution — DIMENSION: the source-scan lane.
//!
//! THE CONTRACT under test: `:i` on a name that is neither a session binding
//! nor an effect type resolves against the stdlib/library SOURCES the session
//! compiles against (`introspect::stdlib_info` over the base include dirs):
//!   - `:i Proc` / `:i Hit` return the real `Tidepool.Records` declaration
//!     with fields AND types verbatim (`exitCode :: Int`, …), `source: "stdlib"`,
//!   - a constructor-only name (`UpdateNoChange`) returns its ENCLOSING data
//!     declaration plus a `constructor` key,
//!   - a session-declared type (`data Doc = Doc Int`) still SHADOWS the
//!     stdlib `Doc` (`source: "session"`),
//!   - a total miss carries the self-explaining `hint` alongside the error.
//!
//! Each test drives the REAL `tidepool-repl` MCP entry point (`dispatch_tool`)
//! through the shared harness (`common::*`), whose `base_include` points at the
//! in-repo `haskell/lib` — so these hits come from the actual `Records.hs`.

mod common;
use common::*;

/// Parse a meta turn's text body as JSON (meta commands emit pure JSON).
fn parse_meta(text: &str) -> serde_json::Value {
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("expected JSON meta body, got:\n{text}\nparse err: {e}"))
}

// ---------------------------------------------------------------------------
// Case 1 — `:i Proc` resolves the stdlib record with fields + types verbatim.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_resolves_stdlib_proc() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.cmd(":i Proc").await;
    let v = parse_meta(t.expect_ok(":i Proc"));
    assert_eq!(v["name"], "Proc", "name echoed: {v}");
    assert_eq!(v["source"], "stdlib", "resolved via the source scan: {v}");
    assert_eq!(v["module"], "Tidepool.Records", "module from header: {v}");
    let shape = v["shape"].as_str().expect("shape is a string");
    for field in ["exitCode :: Int", "stdout :: Text", "stderr :: Text"] {
        assert!(shape.contains(field), "shape must carry `{field}`: {shape}");
    }
    assert!(
        v["file"].as_str().unwrap().ends_with("Records.hs"),
        "file points at the defining source: {v}"
    );

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 2 — `:i Hit` (the other Records vocabulary type) resolves too.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_resolves_stdlib_hit() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.cmd(":i Hit").await;
    let v = parse_meta(t.expect_ok(":i Hit"));
    assert_eq!(v["source"], "stdlib", "{v}");
    let shape = v["shape"].as_str().expect("shape is a string");
    for field in ["path :: Text", "line :: Int", "text :: Text"] {
        assert!(shape.contains(field), "shape must carry `{field}`: {shape}");
    }

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 3 — a constructor-only name returns the ENCLOSING declaration plus the
// `constructor` key (`UpdateNoChange` is a variant of `UpdateOutcome`).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_constructor_only_hit() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.cmd(":i UpdateNoChange").await;
    let v = parse_meta(t.expect_ok(":i UpdateNoChange"));
    assert_eq!(v["constructor"], "UpdateNoChange", "{v}");
    assert_eq!(v["source"], "stdlib", "{v}");
    let shape = v["shape"].as_str().expect("shape is a string");
    assert!(
        shape.starts_with("data UpdateOutcome"),
        "shape is the enclosing decl: {shape}"
    );

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 4 — a session-declared type SHADOWS the stdlib hit: after
// `data Doc = Doc Int`, `:i Doc` reports source "session" (not the
// `Tidepool.Records` Doc).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_decl_shadows_stdlib() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Before the session decl, Doc resolves from the stdlib.
    let t = repl.cmd(":i Doc").await;
    let v = parse_meta(t.expect_ok(":i Doc (pre-decl)"));
    assert_eq!(v["source"], "stdlib", "pre-decl Doc is the stdlib one: {v}");

    repl.def("data Doc = Doc Int").await.expect_ok("decl Doc");
    let t = repl.cmd(":i Doc").await;
    let v = parse_meta(t.expect_ok(":i Doc (post-decl)"));
    assert_eq!(v["source"], "session", "session decl wins: {v}");
    assert!(
        v["shape"].as_str().unwrap().contains("Doc Int"),
        "shape is the session declaration: {v}"
    );

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 5 — a total miss keeps the error AND the self-explaining hint naming
// every lane that was searched.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_miss_carries_hint() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.cmd(":i Nonexistent").await;
    let v = parse_meta(&t.text);
    assert_eq!(v["error"], "not a bound value or known type", "{v}");
    assert_eq!(v["name"], "Nonexistent", "{v}");
    let hint = v["hint"].as_str().expect("miss carries a hint");
    assert!(
        hint.contains("stdlib") && hint.contains(":t"),
        "hint names the searched lanes and the :t affordance: {hint}"
    );

    repl.close().await.expect_ok("close");
}
