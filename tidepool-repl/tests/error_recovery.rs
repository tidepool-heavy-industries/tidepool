//! Wave-3b hardening — DIMENSION: error paths & recovery.
//!
//! THE INVARIANT: every failing turn must return a CLEAN MCP error
//! (`is_error == true`) AND leave the session USABLE — a following good turn
//! must succeed. No panic, no process death, no SIGSEGV/SIGILL, no hang.
//!
//! Each test drives the REAL `tidepool-repl` MCP entry point (`dispatch_tool`)
//! through the shared harness (`common::*`), multi-turn: trigger the failure →
//! assert graceful error → run a known-good turn → assert the session survived.
//!
//! Requires the Wave-3b session-aware `tidepool-extract` (`TIDEPOOL_EXTRACT` +
//! with-packages GHC libdir); skips cleanly otherwise. stderr noise like
//! `Could not find module …Val.G…` is expected and ignored.

mod common;
use common::*;

/// Case 1 — Undefined variable reference.
/// `y` is never bound; referencing it must fail gracefully, and a later
/// reference to the genuinely-bound `x` must still resolve.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undefined_var_then_recover() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // root a real binding first so the session has live state to survive with.
    repl.eval("x <- pure (1 :: Int)")
        .await
        .expect_ok("bind x");

    // reference an undefined var — GHC scope error, folded into a clean MCP error.
    let t = repl.eval("y + 1").await;
    t.expect_err("undefined var y");

    // session survives: the real binding still resolves.
    let t = repl.eval("x + 1").await;
    assert!(t.contains("2"), "post-error x + 1 should be 2: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// Case 2 — Type error in an expression.
/// `True + (1::Int)` is ill-typed; must fail gracefully, then a well-typed turn
/// must succeed on the same session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn type_error_then_recover() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl.eval("pure (True + (1 :: Int))").await;
    t.expect_err("type error");

    let t = repl.eval("pure (1 :: Int)").await;
    assert!(t.contains("1"), "post-type-error pure 1: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// Case 3 — Bad declaration (does a bad decl POISON later turns?).
/// `data = oops` is a syntactically-bad decl. After it errors, a GOOD decl +
/// eval must work. If the bad text is retained in the decl log and breaks every
/// subsequent compile, that is a BUG — see assertions below. `:reset` recovery
/// is also exercised.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_decl_then_recover() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // `data = oops` — not a valid declaration.
    let t = repl.def("data = oops").await;
    t.expect_err("bad decl");

    // A good decl must compile after a bad one. If the bad decl POISONED the
    // decl log, this errors → BUG (bad-decl text retained breaks later compiles).
    let good = repl.def("good x = x + (1 :: Int)").await;
    if good.is_error {
        // BUG: bad decl poisoned the decl log — a subsequent good decl fails to
        // compile because the bad text is retained in the Lane-A generation.
        // Try `:reset` as a recovery path and document whether it clears it.
        let reset = repl.cmd(":reset").await;
        reset.expect_ok("reset after poisoned decl log");
        let good2 = repl.def("good x = x + (1 :: Int)").await;
        good2.expect_ok("good decl AFTER reset (recovery path)");
        let ev = repl.eval("pure (good 5)").await;
        assert!(ev.contains("6"), "post-reset good 5: {}", ev.text);
        panic!(
            "BUG: bad decl `data = oops` poisoned the decl log; good decl only \
             compiled after :reset. First good-decl error: {}",
            good.text
        );
    }

    // No poison: good decl compiled directly; the defined fn is callable.
    let ev = repl.eval("pure (good 5)").await;
    assert!(ev.contains("6"), "good 5 should be 6: {}", ev.text);

    repl.close().await.expect_ok("close");
}

/// Case 4 — Bind of bottom (KEY robustness).
/// `x <- pure (error "boom" :: Int)` — a Tier-0 Int bind deep-forces to NF and
/// hits the `error` thunk. EXPECT a clean MCP error, NOT a crash/SIGILL/hang.
/// Then the session must survive a good turn. Isolated as its own test so that
/// IF it crashes/hangs the binary, the other cases still run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_of_bottom_is_clean() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Tier-0 strict-force of an `error` thunk: must yield a clean error.
    // Observed clean error: `bind runtime error: yield error: Haskell error: boom`
    // (the `error` message propagates as a yield — no SIGILL, no deep_force
    // sentinel crash).
    let t = repl.eval("x <- pure (error \"boom\" :: Int)").await;
    t.expect_err("bind of bottom");

    // session survives.
    let t = repl.eval("pure (1 :: Int)").await;
    assert!(t.contains("1"), "post-bottom-bind pure 1: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// Case 5 — Deep non-tail recursion → clean yield.
/// `countDeep 2_000_000` is non-tail; per project docs the JIT yields a clean
/// "stack overflow / unbounded recursion" error ~10-20k frames, NOT a SIGSEGV.
/// Then the session must survive. Isolated so a hang/crash doesn't take the
/// binary down with it. (NOT an infinite list — that truly hangs.)
///
/// NOTE: the decl name must NOT collide with a `Tidepool.Prelude` export — a
/// session decl is imported UNQUALIFIED alongside the (lens-heavy) Prelude, so
/// e.g. `deep` collides with `Control.Lens.Plated.deep` → an *ambiguous
/// occurrence* COMPILE error before the recursion ever runs (a real usability
/// sharp edge). `countDeep` is collision-free.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_recursion_yields_cleanly() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.def("countDeep n = if n == (0 :: Int) then (0 :: Int) else 1 + countDeep (n - 1)")
        .await
        .expect_ok("def countDeep");

    // 2_000_000 frames comfortably exceeds the ~10-20k non-tail limit. Observed
    // clean yield: `runtime error: yield error: stack overflow (likely infinite
    // list or unbounded recursion …)` — NOT a SIGSEGV.
    let t = repl.eval("pure (countDeep 2000000)").await;
    t.expect_err("deep recursion should yield a clean error");

    // session survives the yield.
    let t = repl.eval("pure (1 :: Int)").await;
    assert!(t.contains("1"), "post-deep-recursion pure 1: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// Case 6 — Empty / whitespace eval.
/// `""` and `   ` must be handled gracefully (error or no-op, never a panic),
/// and the session must remain usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_and_whitespace_eval() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Empty: graceful (we don't assert error-vs-ok, only that it doesn't panic).
    let _ = repl.eval("").await;
    // Whitespace only.
    let _ = repl.eval("   ").await;

    // session survives whatever the empty turns did.
    let t = repl.eval("pure (1 :: Int)").await;
    assert!(t.contains("1"), "post-empty pure 1: {}", t.text);

    // MANDATORY: close to avoid the WorkerHandle::drop deadlock (see
    // `drop_without_close_deadlocks` below). Without this the test hangs on
    // teardown even though every turn succeeded.
    repl.close().await.expect_ok("close");
}

/// Case 7 — Reference after a failed bind.
/// A bind that throws while forcing (`z <- pure (error "x" :: Int)`) must NOT
/// root `z`; `:bindings` must not list it, and the session must stay usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_root_after_failed_bind() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Bind a real value first so :bindings is non-trivially populated.
    repl.eval("k <- pure (5 :: Int)").await.expect_ok("bind k");

    // Failed bind — forcing the error thunk aborts before the value is rooted.
    let t = repl.eval("z <- pure (error \"x\" :: Int)").await;
    t.expect_err("failed bind z");

    // :bindings must NOT list z (it never rooted); k must remain.
    let b = repl.cmd(":bindings").await;
    let b = b.expect_ok(":bindings after failed bind");
    assert!(
        !b.contains("\"z\""),
        "BUG: :bindings lists z after a failed bind: {b}"
    );
    assert!(b.contains("\"k\""), ":bindings should still list k: {b}");

    // session usable.
    let t = repl.eval("k + 1").await;
    assert!(t.contains("6"), "post-failed-bind k + 1: {}", t.text);

    repl.close().await.expect_ok("close");
}

/// REGRESSION — dropping a session WITHOUT `session_close` must NOT hang.
///
/// History: `WorkerHandle::drop` (tidepool-repl/src/worker.rs) used to call
/// `t.join()` BEFORE `cmd_tx` was dropped. Rust drops struct fields only after
/// `Drop::drop` returns, so the sender was still alive during join(); the worker
/// thread, parked in `rx.recv()` (which returns `Err` only once EVERY sender
/// drops), never woke → join blocked forever → teardown deadlock. ANY session
/// not `session_close`'d (a crashed/abandoned MCP client, a panicking turn)
/// would wedge the process on shutdown. Fix: drop/replace `cmd_tx` with a dead
/// sender BEFORE join (mirrors `shutdown()`).
///
/// A hang can't be asserted directly, so the proof is that this test simply
/// COMPLETES under the suite's run timeout: we open a session, run one good
/// turn, then let `Repl` (and thus the server + `WorkerHandle`) drop at end of
/// scope with NO close — and still reach the final assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_without_close_does_not_hang() {
    if !extract_available() {
        return;
    }
    {
        let repl = Repl::new();
        repl.open_ok().await;
        let t = repl.eval("pure (1 :: Int)").await;
        assert!(t.contains("1"), "pure 1: {}", t.text);
        // NO close(): `repl` drops HERE → server → WorkerHandle::drop. Before the
        // fix this deadlocked; now it must return promptly.
    }
    // Reaching this line proves teardown did not hang.
    assert!(true, "server dropped without close and teardown returned");
}
