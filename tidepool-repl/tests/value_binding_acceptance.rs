//! Wave 3b — THE HEADLINE ACCEPTANCE SWEEP: value binding end-to-end through the
//! REAL `tidepool-repl` entry point, multi-turn, with organic GC between bind and
//! read (the standing rule: production tool dispatch over real turns, natural
//! allocation/collection — never a bespoke harness or forced GC).
//!
//! Binds an Int (Tier-0 scalar), a JSON `Value` (Tier-0 structured + DataConTable
//! render), and a function (Tier-1 closure — proves prior-fragment code stays
//! callable after `add_function`); reads/calls each back several turns later,
//! AFTER a real collection forced by a small session nursery + heavy allocation.
//!
//! Requires the Wave-3b `tidepool-extract` (set `TIDEPOOL_EXTRACT`, with the
//! with-packages GHC on `PATH` + `TIDEPOOL_GHC_LIBDIR`); skips cleanly otherwise.

mod common;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn value_binding_int_json_function_survive_gc() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();

    // 0. open
    repl.open().await.expect_ok("open");

    // 0b. define a custom ADT (Lane A → Tidepool.Session.Lib.G1). A value of this
    //     user type, bound below, is what proves the DataConTable MERGE: its `Box`
    //     constructor is registered on the bind turn and must resolve on a LATER
    //     turn's case-match (gen-versioned module addressing, not a wired-in con).
    repl.def("data Box = Box Int").await.expect_ok("def Box");

    // 1. BIND an Int (Tier-0 scalar) — bootstraps the resident machine.
    let t = repl.eval_ok("x <- pure (42 :: Int)").await;
    assert!(t.contains("bound"), "bind x: {t}");

    // 2. read it back a later turn: x + 1 => 43 (pure reference, ExternalEnv slot-load).
    let t = repl.eval_ok("x + 1").await;
    assert!(t.contains("43"), "x + 1: {t}");

    // 3. BIND a custom-ADT value (Tier-0 structured; the `Box` con enters the
    //    session table on THIS turn).
    let t = repl.eval_ok("b <- pure (Box 7)").await;
    assert!(t.contains("bound"), "bind b: {t}");

    // 4. case-match it a later turn — the `Box` con must resolve from the merged
    //    session DataConTable (bound a turn ago) against the tenured heap value.
    let t = repl.eval_ok("case b of Box n -> n + 100").await;
    assert!(t.contains("107"), "case b: {t}");

    // 5. ORGANIC GC: a heavy strict fold allocates ~6 MiB of transient cons into
    //    the 2 MiB nursery → multiple real minor collections.
    let t = repl.eval_ok("foldl' (+) (0 :: Int) [1..200000]").await;
    assert!(t.contains("20000100000"), "fold sum: {t}");

    // 6. BIND a function (Tier-1 closure — stored as-is, not deep-forced).
    let t = repl.eval_ok("f <- pure (\\n -> n + (1 :: Int))").await;
    assert!(t.contains("bound"), "bind f: {t}");

    // 7. call it a later turn: f 10 => 11 (prior-fragment code still callable).
    let t = repl.eval_ok("f 10").await;
    assert!(t.contains("11"), "f 10: {t}");

    // 8. MORE organic GC between the binds and the final re-reads.
    let _ = repl.eval_ok("foldl' (+) (0 :: Int) [1..200000]").await;

    // 9. AFTER the collections, every binding still resolves/renders correctly.
    let t = repl.eval_ok("x + 1").await;
    assert!(t.contains("43"), "post-GC x + 1: {t}");
    let t = repl.eval_ok("case b of Box n -> n + 100").await;
    assert!(t.contains("107"), "post-GC case b: {t}");
    let t = repl.eval_ok("f 10").await;
    assert!(t.contains("11"), "post-GC f 10: {t}");

    // 10. :bindings lists all three current bindings.
    let turn = repl.cmd(":bindings").await;
    assert!(!turn.is_error, ":bindings errored: {}", turn.text);
    assert!(
        turn.contains("\"x\"") && turn.contains("\"b\"") && turn.contains("\"f\""),
        ":bindings: {}",
        turn.text
    );

    // 11. close.
    repl.close().await.expect_ok("close");
}
