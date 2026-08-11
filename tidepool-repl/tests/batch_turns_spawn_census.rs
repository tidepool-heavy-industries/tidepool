//! CENSUS (measurement-only, `batch-turns` baseline). Actual `tidepool-extract`
//! spawns per repl item shape, driven through the REAL `session_run` entry
//! point — the numbers `plans/post-restart/batch-turns-feasibility.md` §1
//! predicts from code reading. This test measures instead of predicting.
//! Changes NOTHING in the compile path; it only reads the process-global
//! spawn counter around real turns.
//!
//! Its own test binary is required: `tidepool_extract_cmd::extract_spawn_count`
//! is PROCESS-GLOBAL (nextest gives one process per test binary). A single
//! `#[test]` drives every shape sequentially against one server so no other
//! test's compiles land on this binary's counter, and so concurrent `#[test]`
//! threads in this same binary never race the counter either.
//!
//! Run: `scripts/battery-shard.sh tidepool-repl` (whole crate), or targeted:
//! `scripts/battery.sh -p tidepool-repl -E 'binary(batch_turns_spawn_census)'`
//! (needs `$TIDEPOOL_EXTRACT` — see the repo `CLAUDE.md`).

mod common;

use common::{build_full_server, require_extract, text_of};
use tidepool_extract_cmd::{extract_spawn_count, reset_extract_spawn_count};
use tidepool_repl::TidepoolReplServer;

/// Dispatch one `session_run` block and report `(all_items_ok, envelope)`.
async fn run_block(server: &TidepoolReplServer, items: &[&str]) -> (bool, serde_json::Value) {
    let mut args = serde_json::Map::new();
    args.insert(
        "items".into(),
        serde_json::Value::Array(
            items
                .iter()
                .map(|s| serde_json::Value::String((*s).to_string()))
                .collect(),
        ),
    );
    let r = server
        .dispatch_tool("session_run", args)
        .await
        .expect("session_run dispatch");
    let raw = text_of(&r);
    let is_error = r.is_error == Some(true);
    let json_part = match raw.rfind("\n## Result\n") {
        Some(pos) => &raw[pos + "\n## Result\n".len()..],
        None => &raw,
    };
    let v: serde_json::Value =
        serde_json::from_str(json_part).unwrap_or_else(|_| serde_json::json!({"raw": raw}));
    let all_ok = v
        .get("items")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .all(|it| it.get("ok").and_then(|o| o.as_bool()).unwrap_or(false))
        })
        .unwrap_or(!is_error);
    (all_ok, v)
}

/// Reset the process-global spawn counter, run one block, and return how many
/// `tidepool-extract` spawns it cost. Asserts the block SUCCEEDED — a failed
/// block takes a different (error-recovery) route with a different spawn
/// count, and would misreport the steady-state cost of the shape.
async fn measure(server: &TidepoolReplServer, label: &str, items: &[&str]) -> u64 {
    reset_extract_spawn_count();
    let (ok, envelope) = run_block(server, items).await;
    let spawns = extract_spawn_count();
    assert!(
        ok,
        "census shape `{label}` must succeed to measure its steady-state cost: {envelope}"
    );
    spawns
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_census_per_item_shape() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf(), "census", true);

    // A. A single decl (unprefixed function equation — the shape used
    // throughout the existing suite, e.g. decl_plane.rs's `inc x = x + 1`).
    let single_decl = measure(&server, "single decl", &["censusDeclA x = x + (1 :: Int)"]).await;

    // B. A run of 3 CONSECUTIVE decls in one block (the batching path at
    // session.rs:904-909).
    let three_consecutive_decls = measure(
        &server,
        "3 consecutive decls",
        &[
            "censusDeclB1 x = x + (1 :: Int)",
            "censusDeclB2 x = x + (2 :: Int)",
            "censusDeclB3 x = x + (3 :: Int)",
        ],
    )
    .await;

    // C. A pure bind (`let x = e`) — routed onto the decl plane by
    // `try_pure_bind_as_decl` (session.rs:1303).
    let pure_bind = measure(&server, "pure bind", &["let censusPure = 41 + (1 :: Int)"]).await;

    // D. An effectful bind (`x <- e`, RHS is a genuine effect) — `run_bind`
    // (session.rs:1711). Needs the full stack's Exec effect.
    let effectful_bind = measure(
        &server,
        "effectful bind",
        &["censusEff <- run \"echo census-effectful\" >>= liftEither"],
    )
    .await;

    // E. A bare expression — `run_bare_expr` (session.rs:2096).
    let bare_expr = measure(&server, "bare expression", &["1 + (1 :: Int)"]).await;

    // F. A mixed 5-item block: decl, pure bind (references the decl),
    // effectful bind, then two bare expressions (referencing the pure bind
    // and the effectful bind's result respectively).
    let mixed_5_item_block = measure(
        &server,
        "mixed 5-item block",
        &[
            "censusMixDecl x = x + (1 :: Int)",
            "let censusMixPure = censusMixDecl 10",
            "censusMixEff <- run \"echo census-mix\" >>= liftEither",
            "censusMixPure",
            "censusMixEff.stdout",
        ],
    )
    .await;

    eprintln!(
        "=== batch-turns spawn census (tidepool-repl/tests/batch_turns_spawn_census.rs) ===\n\
         single decl:              {single_decl}\n\
         3 consecutive decls:      {three_consecutive_decls}\n\
         pure bind:                {pure_bind}\n\
         effectful bind:           {effectful_bind}\n\
         bare expression:          {bare_expr}\n\
         mixed 5-item block:       {mixed_5_item_block}\n\
         ==================================================================="
    );

    // Not a correctness assertion on the exact numbers (that's the whole
    // point — this test's JOB is to report them, not enforce them) but a
    // sanity floor: every measured shape costs at least one real spawn.
    for (label, n) in [
        ("single decl", single_decl),
        ("3 consecutive decls", three_consecutive_decls),
        ("pure bind", pure_bind),
        ("effectful bind", effectful_bind),
        ("bare expression", bare_expr),
        ("mixed 5-item block", mixed_5_item_block),
    ] {
        assert!(
            n >= 1,
            "shape `{label}` reported {n} spawns — expected >= 1"
        );
    }
}
