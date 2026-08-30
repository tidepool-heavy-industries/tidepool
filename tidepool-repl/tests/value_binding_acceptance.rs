//! THE HEADLINE ACCEPTANCE SWEEP: value binding end-to-end through the
//! REAL `tidepool-repl` entry point, multi-turn, with organic GC between bind and
//! read (the standing rule: production tool dispatch over real turns, natural
//! allocation/collection — never a bespoke harness or forced GC).
//!
//! Binds an Int (Tier-0 scalar), a custom-ADT `Box` value (Tier-0 structured +
//! DataConTable render), and a function (Tier-1 closure — proves prior-fragment
//! code stays callable after `add_function`); reads/calls each back a turn
//! later, AFTER a real collection forced by a small session nursery + heavy
//! allocation. (The structured-JSON `Value` bind/read case lives separately in
//! `value_fidelity.rs::structured_json_value_bind_and_read`.)
//!
//! Requires a session-aware `tidepool-extract` (set `TIDEPOOL_EXTRACT`, with the
//! with-packages GHC on `PATH` + `TIDEPOOL_GHC_LIBDIR`); panics loudly otherwise.

mod common;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn value_binding_int_json_function_survive_gc() {
    require_extract();
    let repl = Repl::new();

    // 0. define a custom ADT (auto-opens the session; Lane A → Tidepool.Session.Lib.G1). A value of this
    //     user type, bound below, is what proves the DataConTable MERGE: its `Box`
    //     constructor is registered on the bind turn and must resolve on a LATER
    //     turn's case-match (gen-versioned module addressing, not a wired-in con).
    repl.def("data Box = Box Int").await.expect_ok("def Box");

    // 1. BIND an Int (Tier-0 scalar) — bootstraps the resident machine.
    let t = repl.eval_ok("x <- pure (42 :: Int)").await;
    assert!(t.contains("bound"), "bind x: {t}");

    // 2. BIND a custom-ADT value (Tier-0 structured; the `Box` con enters the
    //    session table on THIS turn).
    let t = repl.eval_ok("b <- pure (Box 7)").await;
    assert!(t.contains("bound"), "bind b: {t}");

    // 3. ORGANIC GC: a heavy strict fold allocates ~6 MiB of transient cons into
    //    the 2 MiB nursery → multiple real minor collections.
    let t = repl.eval_ok("foldl' (+) (0 :: Int) [1..200000]").await;
    assert!(t.contains("20000100000"), "fold sum: {t}");

    // 4. BIND a function (Tier-1 closure — stored as-is, not deep-forced).
    let t = repl.eval_ok("f <- pure (\\n -> n + (1 :: Int))").await;
    assert!(t.contains("bound"), "bind f: {t}");

    // 5. AFTER the collection, every binding resolves/renders correctly on its
    //    first post-bind read: x via pure reference (ExternalEnv slot-load), b's
    //    `Box` con via the merged session DataConTable against the tenured heap
    //    value, f as prior-fragment code still callable.
    let t = repl.eval_ok("x + 1").await;
    assert!(t.contains("43"), "post-GC x + 1: {t}");
    let t = repl.eval_ok("case b of Box n -> n + 100").await;
    assert!(t.contains("107"), "post-GC case b: {t}");
    let t = repl.eval_ok("f 10").await;
    assert!(t.contains("11"), "post-GC f 10: {t}");

    // 6. :bindings lists all three current bindings.
    let turn = repl.cmd(":bindings").await;
    assert!(!turn.is_error, ":bindings errored: {}", turn.text);
    assert!(
        turn.contains("\"x\"") && turn.contains("\"b\"") && turn.contains("\"f\""),
        ":bindings: {}",
        turn.text
    );
}

/// A bare `Either GitError [Commit]` result remains usable in a later turn;
/// callers need not destructure it merely to cross the session boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_error_bare_either_bind_survives_into_next_turn() {
    require_extract();
    let repo_root = tidepool_testing::eval_harness::repo_root();
    let repl = Repl {
        server: build_full_server(repo_root, "giterr", false),
    };

    // Turn 1: bare Either bind — no `Right x <-` destructuring.
    let t = repl.eval_ok("x <- gitLog 2").await;
    assert!(t.contains("bound"), "bind x: {t}");

    // Turn 2: `x :: Either GitError [Commit]` is still a live binding.
    let t = repl
        .eval_ok("pure (either (const (0 :: Int)) length x)")
        .await;
    assert!(t.contains('2'), "either-fold over x in a later turn: {t}");
}
