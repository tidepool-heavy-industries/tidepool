//! Bare-expression compile census: both pure and effectful `it` wrappers are
//! ordered variants in one shared resident turn spawn.
//!
//! Classification failure now stops the block, and every item that reaches
//! evaluation has a real verdict from the block's one `classify_block` spawn.
//! The extractor tries the monadic wrapper first and the pure wrapper second
//! inside that one spawn, preserving never-skip-an-effect ordering without a
//! discarded first diagnostic or a second process.

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

/// A PURE bare expression costs 2 spawns: one block classification and one
/// shared turn compile whose ordered variants select the pure wrapper.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pure_bare_expr_costs_one_turn_compile() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf(), "bare-pure", false);

    let (spawns, is_error, text) = measure_one(&server, "1 + (1 :: Int)").await;
    assert!(!is_error, "pure bare expression turn errored: {text}");
    assert_eq!(
        spawns, 2,
        "pure bare expression should cost 2 spawns (1 classify + 1 ordered-variant \
         turn compile), got {spawns}"
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

/// A failed multiline pure expression is diagnosed against the exact pure
/// wrapper selected by the worker. The missing name is on submission line 2;
/// template placeholders or generated-module coordinates must not leak.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiline_bare_expr_diagnostic_maps_selected_wrapper() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf(), "bare-multiline-diag", false);

    let (is_error, text) = run_single(
        &server,
        "(\n  \\x -> x + missing_multiline_name\n) (1 :: Int)",
        None,
    )
    .await;
    assert!(is_error, "multiline expression should fail: {text}");
    assert!(
        text.contains("<item>:2:"),
        "diagnostic must map to submission line 2: {text}"
    );
    assert!(
        !text.contains("{{TURN") && !text.contains("Expr.hs:"),
        "diagnostic must use the selected rendered wrapper: {text}"
    );
}
