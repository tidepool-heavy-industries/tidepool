//! Wave-3b hardening — DIMENSION: lifecycle & meta-commands.
//!
//! THE CONTRACT under test: the session lifecycle (open / close / reopen) and
//! the meta-command surface (`:bindings`, `:reset`, `:t`, `:i`, and malformed
//! commands — all sent as `:command` items via `session_run`) behave predictably:
//!   - same-name re-open is rejected (a second open for the same session name
//!     without closing the first must error),
//!   - post-close turns report "no session open",
//!   - reopen gives a FRESH session (no leaked bindings),
//!   - `:reset` clears BOTH planes (decl log + value bindings) yet leaves the
//!     session reusable,
//!   - `:bindings` reports the documented JSON shape (name/type/module/tier),
//!   - `:t` / `:i` are IMPLEMENTED (inferred type and binding info respectively),
//!     and malformed / unopened commands fail gracefully (clean error, never a panic).
//!
//! Each test drives the REAL `tidepool-repl` MCP entry point (`dispatch_tool`)
//! through the shared harness (`common::*`), multi-turn. Requires the Wave-3b
//! session-aware `tidepool-extract` (`TIDEPOOL_EXTRACT` + with-packages GHC
//! libdir); skips cleanly otherwise. stderr noise like `Could not find module
//! …Val.G…` is expected and ignored.

mod common;
use common::*;

/// Parse a meta turn's text body as JSON. `:bindings` / `:reset` emit pure JSON
/// (no captured-output prefix), so a direct parse is correct; panic loudly if
/// the shape ever changes (e.g. a stray `## Output` header creeps in).
fn parse_meta(text: &str) -> serde_json::Value {
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("expected JSON meta body, got:\n{text}\nparse err: {e}"))
}

/// Look up a `:bindings` entry by `name` in the parsed meta JSON.
fn binding_entry<'a>(meta: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    meta["bindings"]
        .as_array()
        .expect("bindings is an array")
        .iter()
        .find(|e| e["name"].as_str() == Some(name))
}

// ---------------------------------------------------------------------------
// Case 1 — same-name re-open is rejected.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_open_is_capped() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open().await.expect_ok("first open");
    // Same-name re-open is rejected: a second open without closing the first must error.
    let t = repl.open().await;
    t.expect_err("second open");
    assert!(
        t.contains("already open"),
        "second open should mention 'already open': {}",
        t.text
    );
    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 2 — eval after close reports "no session open".
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eval_after_close_no_session() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;
    // one good turn so the session genuinely existed.
    repl.eval("pure (1 :: Int)").await.expect_ok("good eval");
    repl.close().await.expect_ok("close");

    // post-close: the session manager is empty → clean no-session error.
    // (Multi-session names the session: "no session 'default' open".)
    let t = repl.eval("pure (2 :: Int)").await;
    t.expect_err("eval after close");
    assert!(
        t.contains("no session") && t.contains("open"),
        "post-close eval should report no open session: {}",
        t.text
    );
}

// ---------------------------------------------------------------------------
// Case 3 — close then reopen yields a FRESH session (no leaked bindings).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reopen_is_fresh() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;
    repl.eval("x <- pure (1 :: Int)").await.expect_ok("bind x");
    repl.close().await.expect_ok("close");

    // reopen: a brand-new worker / Session with an empty BindingTable.
    repl.open_ok().await;
    let t = repl.cmd(":bindings").await;
    let meta = parse_meta(t.expect_ok(":bindings on fresh reopen"));
    assert_eq!(
        meta["bindings"].as_array().map(|a| a.len()),
        Some(0),
        "reopened session must have NO bindings: {}",
        t.text
    );
    // x is gone: referencing it is a scope error (folded to a clean MCP error).
    let t = repl.eval("x + 1").await;
    t.expect_err("x should be gone after reopen");

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 4 — `:reset` clears BOTH planes, then the session stays reusable.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_clears_both_planes_and_is_reusable() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Environment: define g; bind v (a pure bind → also lands in the decl
    // plane under the GHCi-environment model, so two generations: g, v).
    repl.def("g x = x + (1 :: Int)").await.expect_ok("def g");
    repl.eval("v <- pure (10 :: Int)").await.expect_ok("bind v");

    // Both live. Confirm via `:bindings` (decl `generation` == 2 for g+v, AND
    // v listed in the unified environment view).
    let meta = parse_meta(repl.cmd(":bindings").await.expect_ok(":bindings pre-reset"));
    assert_eq!(
        meta["generation"].as_u64(),
        Some(2),
        "pre-reset decl generation should be 2 (def g + pure bind v): {meta}"
    );
    assert!(
        binding_entry(&meta, "v").is_some(),
        "pre-reset :bindings should list v: {meta}"
    );

    // reset: drops the machine + clears decl log AND value bindings.
    let t = repl.cmd(":reset").await;
    assert!(
        t.expect_ok(":reset").contains("reset"),
        "reset should ack with 'reset': {}",
        t.text
    );

    // both planes cleared: empty bindings AND decl generation back to 0.
    let meta = parse_meta(
        repl.cmd(":bindings")
            .await
            .expect_ok(":bindings post-reset"),
    );
    assert_eq!(
        meta["bindings"].as_array().map(|a| a.len()),
        Some(0),
        "post-reset :bindings must be empty: {meta}"
    );
    assert_eq!(
        meta["generation"].as_u64(),
        Some(0),
        "post-reset decl generation should be 0: {meta}"
    );
    // references gone: with no binding live these take the PLAIN path → clean
    // scope errors (NOT the reference-path trap). `g`/`v` are both gone.
    repl.eval("pure (g 1)")
        .await
        .expect_err("g should be gone after reset (decl plane cleared)");
    repl.eval("pure (v + 1)")
        .await
        .expect_err("v should be gone after reset (value plane cleared)");

    // REUSABLE post-reset: a fresh decl + plain eval + bind + read all work.
    // `pure (h 5)` runs on the PLAIN path here (no binding live yet) → works.
    repl.def("h x = x * (2 :: Int)")
        .await
        .expect_ok("def h post-reset");
    let t = repl.eval("pure (h 5)").await;
    assert!(
        t.contains("10"),
        "post-reset pure (h 5) should be 10: {}",
        t.text
    );
    repl.eval("w <- pure (3 :: Int)")
        .await
        .expect_ok("bind w post-reset");
    // read a bound VALUE back — the known-good reference path (slot-load).
    let t = repl.eval("w").await;
    assert!(t.contains("3"), "post-reset w should be 3: {}", t.text);

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 5 — `:reset` after a heavy (GC-triggering) turn rebuilds cleanly.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_after_gc_rebuilds() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("a <- pure (5 :: Int)").await.expect_ok("bind a");
    // heavy strict left fold over 200k elements → forces organic GC on the
    // 2 MiB session nursery while the machine is live. BARE form (not
    // `pure (...)`): with `a` live this takes the reference pure-fallback path,
    // which is the known-good route (the `pure (...)` Eff form traps — see the
    // BUG case below).
    let t = repl.eval("foldl' (+) (0 :: Int) [1..200000]").await;
    assert!(
        t.expect_ok("heavy foldl'").contains("20000100000"),
        "heavy foldl' sum should be 20000100000: {}",
        t.text
    );

    // reset drops the (GC'd) machine; the next bind must rebuild it cleanly.
    repl.cmd(":reset").await.expect_ok(":reset after GC");
    repl.eval("b <- pure (7 :: Int)")
        .await
        .expect_ok("rebind b after reset");
    let t = repl.eval("b + 1").await;
    assert!(
        t.contains("8"),
        "post-reset rebuilt machine: b + 1 should be 8: {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 6 — `:bindings` JSON shape (name / type / module / tier; tier per kind).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bindings_shape() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Pure binds under the GHCi-environment model: routed into the decl plane
    // (so they generalize) but STILL surfaced in the unified `:bindings` view.
    repl.eval("x <- pure (1 :: Int)").await.expect_ok("bind x");
    repl.eval("f <- pure (\\n -> n + (1 :: Int))")
        .await
        .expect_ok("bind f");

    let t = repl.cmd(":bindings").await;
    let meta = parse_meta(t.expect_ok(":bindings"));

    let x = binding_entry(&meta, "x").unwrap_or_else(|| panic!("x missing: {meta}"));
    let f = binding_entry(&meta, "f").unwrap_or_else(|| panic!("f missing: {meta}"));

    // Every entry carries the documented keys, and pure binds are decl-backed
    // (module = the Lib.G<g> decl module, tier = DeclBacked — no heap root).
    for (label, e) in [("x", x), ("f", f)] {
        for k in ["name", "type", "module", "tier"] {
            assert!(e.get(k).is_some(), "binding {label} missing key `{k}`: {e}");
        }
        assert!(
            e["module"]
                .as_str()
                .unwrap_or_default()
                .contains("Tidepool.Session.Lib.G"),
            "pure bind {label} lives in the Lib decl module: {e}"
        );
        assert_eq!(
            e["tier"].as_str(),
            Some("DeclBacked"),
            "pure bind {label} is decl-backed: {e}"
        );
    }
    // The generalized type is reported (x :: Int here).
    assert_eq!(x["type"].as_str(), Some("Int"), "x type: {x}");

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 7 — `:t` / `:i` are IMPLEMENTED: `:t` reports an expression's inferred
// type (via the throwaway-bind → type_display path); `:i` reports a bound
// name's type/tier. (Formerly Wave-4 stubs.)
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn type_and_info_are_implemented() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // `:t` returns the inferred type — no longer a stub.
    let t = repl.cmd(":t (1 :: Int)").await;
    let out = t.expect_ok(":t");
    assert!(
        out.contains("Int") && !out.contains("not yet implemented"),
        ":t should report the type Int: {}",
        t.text
    );

    // `:i` on a bound name reports its type.
    repl.eval("b <- pure (True :: Bool)")
        .await
        .expect_ok("bind b");
    let t = repl.cmd(":i b").await;
    assert!(
        t.expect_ok(":i").contains("Bool"),
        ":i b should report Bool: {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 8 — an unknown meta-command returns CallToolResult{is_error:true} with
// "unknown session command". MetaCommand::parse failures route through
// CallToolResult::error (same channel as eval/compile errors), not a
// transport-level McpError.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_meta_command_is_clean_error() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.cmd(":nope").await;
    t.expect_err("unknown :nope should surface as is_error=true");
    assert!(
        t.contains("unknown session command"),
        "error should mention 'unknown session command': {}",
        t.text
    );

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 9 — close when never opened is graceful (clean error, never a panic).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_when_never_opened_is_graceful() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    // No open. Close must NOT panic; it reports no open session.
    let t = repl.close().await;
    t.expect_err("close with no session");
    assert!(
        t.contains("no session"),
        "close-without-open should report 'no session': {}",
        t.text
    );
}

// ---------------------------------------------------------------------------
// Case 10 — `:bindings` on a fresh session: empty list, generation 0.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bindings_on_fresh_session() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.cmd(":bindings").await;
    let meta = parse_meta(t.expect_ok(":bindings on fresh session"));
    assert_eq!(
        meta["bindings"].as_array().map(|a| a.len()),
        Some(0),
        "fresh session should have no bindings: {meta}"
    );
    assert_eq!(
        meta["generation"].as_u64(),
        Some(0),
        "fresh session decl generation should be 0: {meta}"
    );

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 11 — REGRESSION GATE (was BUG-2): the REFERENCE path (engaged whenever
// ANY value binding is live) used to trap with "forced type metadata (should be
// dead code)" for `Eff`-wrapped references and decl-function references, while
// structurally-similar bound-value references worked.
//
// ROOT CAUSE (fixed): `runSessionPipeline` (GhcPipeline) extracted ONLY the turn
// target and resolved every home-library function it called (`object`, `.=`,
// `$fToJSONInt`, `toText`, the Lane-A decl `g`, …) from their HPT interface
// unfoldings. `load'` provisions those ifaces without -O2 unfoldings, so
// `resolveExternals` could not inline them → it baked a poison ErrorSentinel for
// each, and `translateModuleClosed`'s `trulyUnresolved` filter (keyed on the
// un-poisoned id, which never appears once replaced by the sentinel) hid the
// failure → the sentinel fired at run as `kind=4 TypeMetadata`. The fix
// recompiles every home module to full -O2 guts (the one-shot path's approach),
// so no library function is ever left unresolved.
//
// This now asserts the CORRECT behavior (`pure (g 1)` => 2). A regression flips
// it back to the kind=4 trap.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reference_path_type_metadata_trap() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("g x = x + (1 :: Int)").await.expect_ok("def g");
    repl.eval("x <- pure (1 :: Int)").await.expect_ok("bind x");

    // KNOWN-GOOD: referencing the BOUND VALUE on the reference path works.
    let ok = repl.eval("x + 1").await;
    assert!(
        ok.contains("2"),
        "control: `x + 1` (bound-value reference) should be 2: {}",
        ok.text
    );

    // FIXED (BUG-2): referencing the decl FUNCTION `g` on the Eff reference path
    // with a binding live now yields the correct value. The kind=4 TypeMetadata
    // trap was caused by the session extract resolving home-library functions
    // (here `g` from the Lib.G<g> decl module, plus the JSON/Text helpers it and
    // the eval wrapper pull in) from -O0 HPT interface unfoldings, which
    // `resolveExternals` could not inline → poison ErrorSentinels baked into the
    // fragment. `runSessionPipeline` now recompiles every home module to full
    // -O2 guts (GhcPipeline), so no library function is left unresolved.
    let ok = repl.eval("pure (g 1)").await;
    assert!(
        ok.expect_ok("`pure (g 1)` on the reference path with a live binding")
            .contains("2"),
        "`pure (g 1)` should be 2: {}",
        ok.text
    );

    repl.close().await.expect_ok("close");
}

/// `:program` repaints the session as a replayable notebook — declarations
/// in order, then binds with their defining text (the compaction-seam
/// primitive). The emitted program must round-trip: replaying it into a fresh
/// session reproduces the same value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn program_repaint_round_trips() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("dbl x = x * (2 :: Int)")
        .await
        .expect_ok("def dbl");
    repl.eval("x <- pure (dbl 21)").await.expect_ok("bind x");

    let prog = repl.cmd(":program").await;
    let text = prog.expect_ok(":program");
    // Contains the function decl and the pure bind (which lives in the decl
    // plane now, so it appears as `x = (dbl 21)` — the GHCi-environment form).
    assert!(
        text.contains("dbl x = x * (2 :: Int)"),
        "decl missing: {text}"
    );
    assert!(
        text.contains("x =") && text.contains("dbl 21"),
        "pure bind missing from program: {text}"
    );

    // Replay into a fresh session reproduces the value.
    let fresh = Repl::new();
    fresh.open_ok().await;
    fresh
        .def("dbl x = x * (2 :: Int)")
        .await
        .expect_ok("replay def");
    fresh
        .eval("x <- pure (dbl 21)")
        .await
        .expect_ok("replay bind");
    let out = fresh.eval_ok("pure x").await;
    assert!(out.contains("42"), "replay value: expected 42, got {out}");

    repl.close().await.expect_ok("close");
    fresh.close().await.expect_ok("close fresh");
}

/// Redefining a decl that a live bind referenced reports the bind as `stale`
/// (notebook-frame display truthfulness — the value doesn't recompute).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redefine_reports_stale_binds() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("factor x = x * (2 :: Int)")
        .await
        .expect_ok("def factor");
    repl.eval("y <- pure (factor 10)")
        .await
        .expect_ok("bind y = factor 10");

    // Redefine factor — y still holds the OLD value; the response must say so.
    let redef = repl.def("factor x = x * (100 :: Int)").await;
    let text = redef.expect_ok("redefine factor");
    assert!(text.contains("stale"), "expected stale marker: {text}");
    assert!(text.contains('y'), "expected bind y named stale: {text}");

    // A redefine touching nothing bound reports no stale key.
    let unrelated = repl.def("other x = x + (1 :: Int)").await;
    let ut = unrelated.expect_ok("def other");
    assert!(
        !ut.contains("stale"),
        "unrelated redefine should not be stale: {ut}"
    );

    repl.close().await.expect_ok("close");
}
