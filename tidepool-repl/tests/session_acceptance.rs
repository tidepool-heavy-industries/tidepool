//! Wave-2 acceptance — the real `tidepool-repl` entry point, multi-turn, on one
//! resident machine (the standing rule: drive the production tool dispatch over
//! several real turns, not a bespoke harness).
//!
//! Flow: session_run (def `slug`, auto-opens) → session_run (eval `slug "a b"`)
//! → "a-b" → a SECOND session_run on the SAME machine (heap persists, re-entry
//! via `add_function`/`run_fragment`) → session_reset (drops the machine + all
//! bindings) → `slug` is gone.
//!
//! Requires `tidepool-extract` (the GHC→Core extractor) on `$PATH` or via
//! `TIDEPOOL_EXTRACT`; skips cleanly otherwise.

mod common;

use common::{build_server_with_nursery, extract_available, text_of};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_multi_turn_real_path() {
    if !extract_available() {
        eprintln!(
            "skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT / run in nix develop)"
        );
        return;
    }
    let repl = common::Repl {
        server: build_server_with_nursery(None, None, None),
    };

    // 1. define `slug` (Lane A → Tidepool.Session.Lib.G1) — auto-opens the session.
    let turn = repl.def("slug t = T.replace \" \" \"-\" t").await;
    assert!(!turn.is_error, "def errored: {}", turn.text);
    let txt = &turn.text;
    assert!(
        txt.contains("\"decl\""),
        "def: expected slim decl field in: {txt}"
    );

    // 2. eval `slug "a b"` → "a-b" (bootstraps the resident machine)
    let turn = repl.eval("pure (slug \"a b\")").await;
    assert!(!turn.is_error, "eval 1 errored: {}", turn.text);
    assert!(turn.text.contains("a-b"), "eval 1 result: {}", turn.text);

    // 3. a SECOND eval on the SAME machine (re-entry; heap persists)
    let turn = repl.eval("pure (slug \"x y\")").await;
    assert!(!turn.is_error, "eval 2 errored: {}", turn.text);
    assert!(turn.text.contains("x-y"), "eval 2 result: {}", turn.text);

    // 4. reset (drops the machine + all bindings, opens fresh)
    let r = repl
        .server
        .dispatch_tool("session_reset", serde_json::Map::new())
        .await
        .expect("session_reset");
    assert_ne!(r.is_error, Some(true), "reset errored: {}", text_of(&r));
    assert!(text_of(&r).contains("reset"));

    // after reset the fresh session has no `slug` — referencing it is a scope error.
    let turn = repl.eval("pure (slug \"a b\")").await;
    assert!(turn.is_error, "post-reset eval should error (slug is gone)");
    assert!(
        turn.text.contains("slug") || turn.text.to_lowercase().contains("scope"),
        "post-reset error should be a scope error for slug, got: {}",
        turn.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_from_cold_start_then_run() {
    if !extract_available() {
        return;
    }
    let repl = common::Repl {
        server: build_server_with_nursery(None, None, None),
    };
    // reset as the FIRST call (no session yet) must succeed and leave a fresh,
    // runnable session behind.
    let r = repl
        .server
        .dispatch_tool("session_reset", serde_json::Map::new())
        .await
        .expect("session_reset");
    assert_ne!(
        r.is_error,
        Some(true),
        "cold reset errored: {}",
        text_of(&r)
    );
    assert!(text_of(&r).contains("reset"));

    let turn = repl.eval("pure (1 :: Int)").await;
    assert!(
        !turn.is_error,
        "run after cold reset errored: {}",
        turn.text
    );
    assert!(turn.text.contains('1'), "run after reset: {}", turn.text);
}
