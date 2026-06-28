//! Wave-3b hardening — DIMENSION: lifecycle & meta-commands.
//!
//! THE CONTRACT under test: the session lifecycle (open / close / reopen) and
//! the `session_cmd` meta surface (`:bindings`, `:reset`, the `:t` / `:i`
//! Wave-4 stubs, and malformed commands) behave predictably:
//!   - cap = 1 (a second open without close is rejected),
//!   - post-close turns report "no session open",
//!   - reopen gives a FRESH session (no leaked bindings),
//!   - `:reset` clears BOTH planes (decl log + value bindings) yet leaves the
//!     session reusable,
//!   - `:bindings` reports the documented JSON shape (name/type/module/tier),
//!   - `:t` / `:i` are KNOWN Wave-4 stubs (codified so a future implementer
//!     flips the assertion), and malformed / unopened commands fail gracefully
//!     (clean error, never a panic).
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
// Case 1 — double open is capped (MVP cap = 1).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_open_is_capped() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open().await.expect_ok("first open");
    // A second open without closing the first must error (cap = 1).
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

    // post-close: the session manager is empty → clean "no session open".
    let t = repl.eval("pure (2 :: Int)").await;
    t.expect_err("eval after close");
    assert!(
        t.contains("no session open"),
        "post-close eval should report 'no session open': {}",
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

    // decl plane: define g; value plane: bind v.
    repl.def("g x = x + (1 :: Int)").await.expect_ok("def g");
    repl.eval("v <- pure (10 :: Int)").await.expect_ok("bind v");

    // Both planes live. We confirm this via `:bindings` (decl `generation` == 1
    // AND value binding `v` listed) rather than by EVALUATING `g 1`: with a
    // binding live, referencing the Lane-A decl FUNCTION `g` traps with
    // "forced type metadata" — a CONFIRMED BUG codified in
    // `eff_wrapped_reference_traps_with_live_binding` below. Using the meta-plane
    // keeps this lifecycle test focused on :reset rather than tripping that bug.
    let meta = parse_meta(repl.cmd(":bindings").await.expect_ok(":bindings pre-reset"));
    assert_eq!(
        meta["generation"].as_u64(),
        Some(1),
        "pre-reset decl generation should be 1 (def g): {meta}"
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
    let meta = parse_meta(repl.cmd(":bindings").await.expect_ok(":bindings post-reset"));
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
    repl.def("h x = x * (2 :: Int)").await.expect_ok("def h post-reset");
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
    let t = repl
        .eval("foldl' (+) (0 :: Int) [1..200000]")
        .await;
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

    repl.eval("x <- pure (1 :: Int)").await.expect_ok("bind x");
    repl.eval("f <- pure (\\n -> n + (1 :: Int))")
        .await
        .expect_ok("bind f");

    let t = repl.cmd(":bindings").await;
    let meta = parse_meta(t.expect_ok(":bindings"));

    let x = binding_entry(&meta, "x").unwrap_or_else(|| panic!("x missing: {meta}"));
    let f = binding_entry(&meta, "f").unwrap_or_else(|| panic!("f missing: {meta}"));

    // Every entry carries the documented keys.
    for (label, e) in [("x", x), ("f", f)] {
        for k in ["name", "type", "module", "tier"] {
            assert!(
                e.get(k).is_some(),
                "binding {label} missing key `{k}`: {e}"
            );
        }
        // both rooted under a Tidepool.Session.Val.G<g> module.
        assert!(
            e["module"]
                .as_str()
                .unwrap_or_default()
                .contains("Tidepool.Session.Val.G"),
            "binding {label} module should be a Val.G<g>: {e}"
        );
    }

    // tier classification: a forced data value vs a closure.
    assert_eq!(
        x["tier"].as_str(),
        Some("Tier0Data"),
        "x (pure Int) should be Tier0Data: {x}"
    );
    assert_eq!(
        f["tier"].as_str(),
        Some("Tier1Closure"),
        "f (a lambda) should be Tier1Closure: {f}"
    );

    repl.close().await.expect_ok("close");
}

// ---------------------------------------------------------------------------
// Case 7 — `:t` / `:i` are KNOWN Wave-4 stubs (codify the current contract).
// When Wave 4 lands these will need to flip to real type/info output.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn type_and_info_are_wave4_stubs() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // `:t` and `:i` are NOT errors today — they return a "not yet implemented"
    // note (TurnOutcome::Meta). This is a tracked Wave-4 gap, not a bug.
    let t = repl.cmd(":t foo").await;
    assert!(
        t.expect_ok(":t stub").contains("not yet implemented"),
        ":t should return the Wave-4 stub note: {}",
        t.text
    );
    let t = repl.cmd(":i foo").await;
    assert!(
        t.expect_ok(":i stub").contains("not yet implemented"),
        ":i should return the Wave-4 stub note: {}",
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
// Case 11 — CONFIRMED BUG: the REFERENCE path (engaged whenever ANY value
// binding is live) traps with "forced type metadata (should be dead code)" for
// certain reference expressions, while structurally-similar ones work.
//
// TRAP TRIGGERS (both reproduced, both yield `kind=4 (TypeMetadata)` + `[CASE
// TRAP]` + runtime error "forced type metadata (should be dead code)"):
//   (A) any `Eff`-wrapped reference — even prelude-only: with a binding live,
//       `pure (foldl' (+) (0::Int) [1..5])` traps (the bare `foldl' …` does NOT).
//   (B) referencing a Lane-A decl FUNCTION — `g 1` (for `g x = x + 1`) traps
//       BOTH bare AND as `pure (g 1)`, with a binding live.
//
// KNOWN-GOOD on the SAME reference path (from value_binding_acceptance, green):
//   - bare reference to a BOUND VALUE: `x + 1`, `f 10`
//   - `case`-match on a bound ADT value: `case b of Box n -> n + 100`
//   - bare prelude-only expression: `foldl' (+) (0::Int) [1..200000]`
//
// OBSERVED vs EXPECTED: `pure (g 1)` should yield 2 (it does on the PLAIN path
// when NO binding is live — cf. session_acceptance `pure (slug …)`). The
// reference fragment forces a dead type-metadata thunk instead.
//
// ROOT CAUSE (file:line): tidepool-repl/src/session.rs:410–488
// (`run_session_reference` / `run_reference_fragment`) — the reference fragment
// is compiled via `compile_session_turn` with injected `Val` ifaces + the merged
// `session_table`; for triggers (A)/(B) the JIT forces a type-metadata thunk
// that the slot-load (bound-value) references don't hit.
//
// This test asserts the CURRENT (broken) behavior so the suite stays green and a
// future fixer SEES it flip: when fixed, the `expect_err` below should become
// `expect_ok` + `contains("2")`.
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

    // BUG trigger (B): referencing the decl FUNCTION `g` traps. (Should be 2.)
    let trap = repl.eval("pure (g 1)").await;
    trap.expect_err("BUG: `pure (g 1)` traps on the reference path with a live binding");
    assert!(
        trap.contains("type metadata"),
        "BUG repro: `pure (g 1)` should trap with a TypeMetadata yield error: {}",
        trap.text
    );

    repl.close().await.expect_ok("close");
}
