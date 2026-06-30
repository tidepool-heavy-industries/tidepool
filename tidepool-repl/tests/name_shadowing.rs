//! Name-collision shadowing: a session-owned name must SHADOW a same-named
//! Prelude / `Library` / effect-verb import, not become a GHC "Ambiguous
//! occurrence" that fails the turn AND poisons every later turn (the colliding
//! import is regenerated each turn). Regression for the footgun found while
//! dogfooding: `let glob = …` collided with the `Fs` `glob` verb and wedged the
//! whole session until `:reset`.
//!
//! The minimal test stack has no `Fs`/`Library`, so we exercise the same
//! mechanism against a Prelude re-export: before the fix, value-plane bind names
//! were never added to the import `hiding (…)` clause (only declaration binders
//! were), so binding a Prelude name and then using it was ambiguous.
//!
//! Requires `TIDEPOOL_EXTRACT`; skips cleanly otherwise.

mod common;
use common::*;

/// `let lookup = …` binds a value whose name collides with `Prelude.lookup`.
/// Using `lookup` on a LATER turn must resolve to the binding (the session name
/// shadows the import), not raise an ambiguous occurrence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn value_bind_shadows_prelude_name() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let bind = repl.eval("let lookup = (42 :: Int)").await;
    assert!(
        !bind.is_error,
        "binding a value named `lookup` should succeed: {}",
        bind.text
    );

    // The poison this fix removes: `lookup` here was ambiguous between the
    // session's `Val.*.lookup` and `Prelude.lookup` (both imported unqualified).
    let used = repl.eval("pure (lookup + 1)").await;
    assert!(
        !used.is_error,
        "a bound name must shadow the Prelude re-export, not poison the turn: {}",
        used.text
    );
    assert!(
        used.text.contains("43"),
        "shadowed binding should evaluate (42 + 1): {}",
        used.text
    );

    // And the session is not wedged — a subsequent unrelated turn still works.
    let after = repl.eval("pure (length [1, 2, 3 :: Int])").await;
    assert!(
        !after.is_error,
        "session should remain usable after a shadowed name: {}",
        after.text
    );
    assert!(after.text.contains('3'), "follow-up value: {}", after.text);

    repl.close().await;
}
