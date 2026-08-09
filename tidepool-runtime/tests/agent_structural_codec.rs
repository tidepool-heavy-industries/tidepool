//! PRD 18 gate 1(b): prove a list-carrying type AND a genuinely RECURSIVE
//! ADT round-trip through `Tidepool.Agent.CodecSpike`'s structural codec on
//! the real extract/JIT — the polarity the operator-form interpreter
//! rejects and this interpreter requires. See
//! `plans/post-restart/agent-lanes/dev-structural-codec.md`.
//!
//! Every test drives `encodeS`/`decodeS`/`roundTrips` from
//! `Tidepool.Agent.CodecSpike` through a real compile + JIT run
//! (`EvalHarness::run_pure`) — the round-tripped value is reconstructed by
//! code that actually executed, not merely typechecked.
//!
//! Requires a worktree extract binary (`cabal build tidepool-extract-bin`,
//! then `TIDEPOOL_EXTRACT` pointed at it, or run inside `nix develop`). Panics
//! loudly (see `require_extract`) when the extractor is unreachable.

use serde_json::json;
use tidepool_testing::eval_harness::{require_extract, EvalHarness};

/// Compile + run a full module PURE on the JIT, returning the target binding's
/// JSON. Panics loudly when the extractor is unavailable.
fn run(source: &str, target: &str) -> serde_json::Value {
    require_extract();
    EvalHarness::new()
        .with_stdlib()
        .run_pure(source, target)
        .expect("compile_and_run_pure failed")
        .to_json()
}

const HEADER: &str = "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}\n\
                      module Expr where\n\
                      import Tidepool.Prelude hiding (error)\n\
                      import Tidepool.Agent.CodecSpike\n";

/// Shape 1 (list-carrying record-and-sum), non-empty list: `Completed`'s
/// `caveats` round-trips through encode/decode.
#[test]
fn worker_result_completed_round_trips() {
    let src = format!(
        "{HEADER}\n\
         result :: Bool\n\
         result = roundTrips (Completed \"done\" [\"caveat one\", \"caveat two\"])\n"
    );
    let v = run(&src, "result");
    assert_eq!(
        v,
        json!(true),
        "Completed with a non-empty list round-trips"
    );
}

/// Shape 1, the sibling constructor: `Blocked` round-trips too — both
/// records of the sum, proving the record case isn't a one-constructor
/// fluke.
#[test]
fn worker_result_blocked_round_trips() {
    let src = format!(
        "{HEADER}\n\
         result :: Bool\n\
         result = roundTrips (Blocked \"waiting on review\" [\"evidence a\", \"evidence b\", \"evidence c\"])\n"
    );
    let v = run(&src, "result");
    assert_eq!(v, json!(true), "Blocked round-trips");
}

/// Edge case: an EMPTY list field round-trips — empty containers are where
/// codecs quietly disagree (an omitted field, a null, vs. a real `[]`).
#[test]
fn worker_result_empty_list_round_trips() {
    let src = format!(
        "{HEADER}\n\
         result :: Bool\n\
         result = roundTrips (Completed \"done\" [])\n"
    );
    let v = run(&src, "result");
    assert_eq!(
        v,
        json!(true),
        "Completed with an empty caveats list round-trips"
    );
}

/// The wire shape itself, not just the round-trip boolean: `Completed` and
/// `Blocked` — a record sum — both encode to the SAME `{"tag", "fields"}`
/// shape, proving the codec doesn't special-case one constructor form over
/// another.
#[test]
fn worker_result_encodes_to_uniform_tagged_shape() {
    let src = format!(
        "{HEADER}\n\
         result :: Value\n\
         result = encodeS (Completed \"done\" [\"c1\"])\n"
    );
    let v = run(&src, "result");
    assert_eq!(
        v,
        json!({"tag": "Completed", "fields": ["done", ["c1"]]}),
        "Completed encodes with fields in declaration order"
    );
}

/// Shape 2 (genuinely recursive ADT, recursion through a list): a single
/// leaf `Step` round-trips.
#[test]
fn plan_leaf_round_trips() {
    let src = format!(
        "{HEADER}\n\
         result :: Bool\n\
         result = roundTrips (Step \"leaf\")\n"
    );
    let v = run(&src, "result");
    assert_eq!(v, json!(true), "Step leaf round-trips");
}

/// Edge case: recursion through an EMPTY list — `Seq []` — round-trips. This
/// is the sharpest version of the empty-container edge because it sits
/// directly on the recursive knot (`Structural Plan` needing
/// `Structural [Plan]` needing `Structural Plan`) rather than a leaf field.
#[test]
fn plan_empty_seq_round_trips() {
    let src = format!(
        "{HEADER}\n\
         result :: Bool\n\
         result = roundTrips (Seq [])\n"
    );
    let v = run(&src, "result");
    assert_eq!(v, json!(true), "Seq [] round-trips");
}

/// Edge case: a value nested at least two levels deep — `Seq` containing a
/// `Step` AND a nested `Seq` — round-trips. Depth-1-vs-depth-N is the other
/// place codecs quietly disagree.
#[test]
fn plan_nested_depth_two_round_trips() {
    let src = format!(
        "{HEADER}\n\
         result :: Bool\n\
         result = roundTrips (Seq [Step \"a\", Seq [Step \"b\", Step \"c\"]])\n"
    );
    let v = run(&src, "result");
    assert_eq!(v, json!(true), "a two-level-deep Seq round-trips");
}

/// The wire shape for the nested-depth-two `Plan`, reconstructed by code
/// that actually ran: proves the recursive encode isn't accidentally flat
/// or truncated at depth 1.
#[test]
fn plan_nested_depth_two_encodes_correctly() {
    let src = format!(
        "{HEADER}\n\
         result :: Value\n\
         result = encodeS (Seq [Step \"a\", Seq [Step \"b\", Step \"c\"]])\n"
    );
    let v = run(&src, "result");
    assert_eq!(
        v,
        json!({
            "tag": "Seq",
            "fields": [[
                {"tag": "Step", "fields": ["a"]},
                {"tag": "Seq", "fields": [[
                    {"tag": "Step", "fields": ["b"]},
                    {"tag": "Step", "fields": ["c"]}
                ]]}
            ]]
        }),
        "nested Seq encodes with real recursive structure, not a truncation"
    );
}

/// Loud rejection, not silent coercion: an unrecognized tag decodes to
/// `Left`, never a default/sentinel `Plan`.
#[test]
fn unknown_tag_is_a_loud_decode_error() {
    let src = format!(
        "{HEADER}\n\
         result :: Bool\n\
         result = case (decodeS (object [\"tag\" .= (\"Bogus\" :: Text), \"fields\" .= ([] :: [Value])]) :: Either Text Plan) of\n\
         \x20 Left _  -> True\n\
         \x20 Right _ -> False\n"
    );
    let v = run(&src, "result");
    assert_eq!(
        v,
        json!(true),
        "an unrecognized tag must decode to Left, not a default Plan"
    );
}

/// Loud rejection: a field-count mismatch (one field where `Step` expects
/// exactly one, but here `Seq` expects one list field and gets two raw
/// fields) decodes to `Left`.
#[test]
fn wrong_field_count_is_a_loud_decode_error() {
    let src = format!(
        "{HEADER}\n\
         result :: Bool\n\
         result = case (decodeS (object [\"tag\" .= (\"Step\" :: Text), \"fields\" .= ([\"a\", \"b\"] :: [Text])]) :: Either Text Plan) of\n\
         \x20 Left _  -> True\n\
         \x20 Right _ -> False\n"
    );
    let v = run(&src, "result");
    assert_eq!(
        v,
        json!(true),
        "a field-count mismatch must decode to Left, not a truncated/padded Plan"
    );
}
