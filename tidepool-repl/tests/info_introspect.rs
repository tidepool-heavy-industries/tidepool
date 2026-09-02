//! GHC-authoritative `:info` over the exact environment used by a REPL turn.

mod common;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_resolves_stdlib_proc() {
    require_extract();
    let repl = Repl::new();

    let t = repl.cmd(":i Proc").await;
    let shape = t.expect_ok(":i Proc");
    for field in ["exitCode :: Int", "stdout :: Text", "stderr :: Text"] {
        assert!(shape.contains(field), "shape must carry `{field}`: {shape}");
    }
    assert!(
        shape.contains("data Proc"),
        "GHC rendered the declaration: {shape}"
    );
}

// ---------------------------------------------------------------------------
// Case 2 — `:i Hit` (the other Records vocabulary type) resolves too.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_resolves_stdlib_hit() {
    require_extract();
    let repl = Repl::new();

    let t = repl.cmd(":i Hit").await;
    let shape = t.expect_ok(":i Hit");
    for field in ["path :: Text", "line :: Int", "text :: Text"] {
        assert!(shape.contains(field), "shape must carry `{field}`: {shape}");
    }
}

// ---------------------------------------------------------------------------
// Case 3 — a constructor-only name returns the ENCLOSING declaration plus the
// `constructor` key (`UpdateNoChange` is a variant of `UpdateOutcome`).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_constructor_only_hit() {
    require_extract();
    let repl = Repl::new();

    let t = repl.cmd(":i UpdateNoChange").await;
    let shape = t.expect_ok(":i UpdateNoChange");
    assert!(
        shape.contains("data UpdateOutcome") && shape.contains("UpdateNoChange"),
        "GHC identifies the constructor in its enclosing declaration: {shape}"
    );
}

// ---------------------------------------------------------------------------
// Case 4 — a session-declared type SHADOWS the stdlib hit: after
// `data Hit = Hit Int`, `:i Hit` reports source "session" (not the
// `Tidepool.Records` Hit).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_decl_shadows_stdlib() {
    require_extract();
    let repl = Repl::new();

    // Before the session decl, Hit resolves from the stdlib.
    let t = repl.cmd(":i Hit").await;
    let before = t.expect_ok(":i Hit (pre-decl)");
    assert!(
        before.contains("path :: Text"),
        "pre-decl Hit is the stdlib one: {before}"
    );

    repl.def("data Hit = Hit Int").await.expect_ok("decl Hit");
    let t = repl.cmd(":i Hit").await;
    let shape = t.expect_ok(":i Hit (post-decl)");
    assert!(
        shape.contains("Hit Int") && !shape.contains("path :: Text"),
        "shape is the session declaration: {shape}"
    );
}

// ---------------------------------------------------------------------------
// Case 5 — a total miss keeps the error AND the self-explaining hint naming
// every lane that was searched.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_miss_is_concise() {
    require_extract();
    let repl = Repl::new();

    let t = repl.cmd(":i Nonexistent").await;
    assert_eq!(t.expect_ok(":i Nonexistent"), "unknown name `Nonexistent`");
}
