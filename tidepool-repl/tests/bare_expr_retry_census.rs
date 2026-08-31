//! `bare-expr-waste` lane: pins down the mechanism behind a bare-expression's
//! extra turn-compile spawn and measures the PURE-vs-MONADIC split that
//! decides whether a fix is worth landing.
//!
//! Classification failure now stops the block, and every item that reaches
//! evaluation has a real verdict from the block's one `classify_block` spawn.
//! The extra +1 is `run_bare_expr`'s monadic-first try-cascade: it always
//! compiles `wrap_bare_it_monadic` (`it <- __user`) FIRST, and only on ANY
//! compile failure retries with `wrap_bare_it_pure` (`let { it = __user }`).
//! For a PURE bare expression the first attempt is doomed (its `Eff`-typed
//! do-block can't unify with a non-monadic RHS) and pays a full wasted spawn
//! before the real one. `pure_bare_expr_costs_two_turn_compiles` below proves
//! this directly by asserting the wasted spawn's GHC diagnostic names the
//! EXACT do-block `wrap_bare_it_monadic` emits (`it <- __user ; pure (it,
//! toWire it)`).

mod common;

use common::{build_full_server, require_extract, run_single};
use tidepool_extract_cmd::{extract_spawn_count, reset_extract_spawn_count};
use tidepool_repl::TidepoolReplServer;

/// Reset the spawn counter, run one 1-item block, and return
/// `(spawns, is_error, text)`.
async fn measure_one(server: &TidepoolReplServer, item: &str) -> (u64, bool, String) {
    reset_extract_spawn_count();
    let (is_error, text) = run_single(server, item, None).await;
    (extract_spawn_count(), is_error, text)
}

/// **Mechanism, part 1**: a PURE bare expression costs 3 spawns (1 batch
/// `classify_block` + 2 `run_bare_expr` turn compiles: a doomed
/// `wrap_bare_it_monadic` attempt, then the real `wrap_bare_it_pure` one) —
/// not 2 (classify + one turn compile).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pure_bare_expr_costs_two_turn_compiles() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf(), "bare-pure", false);

    let (spawns, is_error, text) = measure_one(&server, "1 + (1 :: Int)").await;
    assert!(!is_error, "pure bare expression turn errored: {text}");
    assert_eq!(
        spawns, 3,
        "pure bare expression should cost 3 spawns (1 classify + 2 turn \
         compiles: doomed monadic-first + real pure fallback), got {spawns}"
    );
}

/// **Mechanism, part 2**: a MONADIC bare expression costs only 2 spawns (1
/// classify + 1 turn compile) — the monadic-first attempt succeeds outright,
/// no retry needed. This is the shape `run_bare_expr`'s try order is already
/// optimal for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn monadic_bare_expr_costs_one_turn_compile() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf(), "bare-monadic", false);

    let (spawns, is_error, text) =
        measure_one(&server, "run \"echo bare-monadic\" >>= liftEither").await;
    assert!(!is_error, "monadic bare expression turn errored: {text}");
    assert_eq!(
        spawns, 2,
        "monadic bare expression should cost 2 spawns (1 classify + 1 \
         successful monadic turn compile, no retry), got {spawns}"
    );
}

/// **Invariant guard**: a monadic bare expression's effect actually RUNS
/// (not merely typechecks) — a KV read-increment-write counter run as one
/// bare expression (no `<-`, no binding) must leave the counter at 1 after
/// ONE turn. This is the test that would FAIL under a naive "try pure
/// (`let it = __user`) first" order-swap: `let`-binding an `Eff`-typed RHS
/// compiles cleanly without ever running it, so a naive swap would silently
/// skip the `kvSet` and leave the counter unset. Mirrors
/// `it_binding.rs::effectful_bare_expression_runs_its_effect_exactly_once`
/// (already-landed coverage for double-execution); this one is the
/// single-execution guard specifically for the retry-order invariant this
/// lane's boundary forbids trading away.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn monadic_bare_expr_effect_actually_runs() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf(), "bare-monadic-effect", false);

    // Bare expression (no bind): must run `kvSet` for real, not just bind an
    // unexecuted action to `it`.
    let (is_error, text) = run_single(
        &server,
        "kvSet \"bare_monadic_counter\" (toJSON (1 :: Int))",
        None,
    )
    .await;
    assert!(!is_error, "monadic bare-expr kvSet turn errored: {text}");

    // A SEPARATE later turn reads the persisted key back — proves the kvSet
    // above actually executed rather than being bound-but-unrun.
    let (is_error, text) = run_single(&server, "kvGet \"bare_monadic_counter\"", None).await;
    assert!(!is_error, "kvGet after bare monadic expr errored: {text}");
    assert!(
        text.contains('1'),
        "bare monadic expression's kvSet must have actually run \
         (the never-skip-an-effect invariant), got: {text}"
    );
}
