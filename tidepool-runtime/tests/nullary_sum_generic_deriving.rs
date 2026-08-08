//! Nullary-sum (enum) `Generic`-derived `ToJSON`/`FromJSON`, executed on the
//! JIT through the real extract pipeline (`EvalHarness::run_pure`).
//!
//! Before this fix, the vendored Aeson's Generic machinery rejected EVERY
//! multi-constructor sum at compile time — including a plain enum like
//! `data Mode = Observing | Deciding | Acting`, forcing authors to hand-write
//! `ToJSON`/`FromJSON` for such fields (see `examples/harness/Harness.hs`'s
//! former `Mode`/`Confidence` instances). Now a NULLARY sum (every
//! constructor has zero fields) derives generically, encoding each
//! constructor as its bare name string; a sum with any non-nullary
//! constructor is still rejected at compile time (unchanged, exercised by
//! `generic_deriving_337.rs::sum_type_rejected_at_compile_time`).
//!
//! Requires a worktree extract binary (`cabal build tidepool-extract-bin`,
//! then `TIDEPOOL_EXTRACT` pointed at it, or run inside `nix develop`). Skips
//! cleanly when the extractor is unreachable.

use serde_json::json;
use tidepool_testing::eval_harness::{extract_available, EvalHarness};

fn run(source: &str, target: &str) -> Option<serde_json::Value> {
    if !tidepool_testing::eval_harness::extract_available() {
        eprintln!("skipping: tidepool-extract unavailable (set TIDEPOOL_EXTRACT / nix develop)");
        return None;
    }
    Some(
        EvalHarness::new()
            .with_stdlib()
            .run_pure(source, target)
            .expect("compile_and_run_pure failed")
            .to_json(),
    )
}

const HEADER: &str =
    "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, DeriveAnyClass #-}\n\
                      module Expr where\n\
                      import Tidepool.Prelude hiding (error)\n";

/// The generic `ToJSON` default encodes a nullary-sum constructor as its bare
/// name string, matching the hand-written `Mode`/`Confidence` instances this
/// change lets authors delete.
#[test]
fn generic_tojson_enum_emits_constructor_name() {
    let src = format!(
        "{HEADER}\n\
         data Mode = Observing | Deciding | Acting deriving (Generic, ToJSON)\n\n\
         result :: Value\n\
         result = toJSON Deciding\n"
    );
    if let Some(v) = run(&src, "result") {
        assert_eq!(
            v,
            json!("Deciding"),
            "enum constructor encodes as its bare name string"
        )
    }
}

/// The generic `FromJSON` default decodes the constructor-name string back
/// into the matching nullary constructor.
#[test]
fn generic_fromjson_enum_decodes_from_name() {
    let src = format!(
        "{HEADER}\n\
         data Mode = Observing | Deciding | Acting deriving (Generic, FromJSON, Eq)\n\n\
         result :: Bool\n\
         result = case (fromJSON (String \"Acting\") :: Result Mode) of\n\
         \x20 Success m -> m == Acting\n\
         \x20 Error _ -> False\n"
    );
    if let Some(v) = run(&src, "result") {
        assert_eq!(
            v,
            json!(true),
            "\"Acting\" decodes to the Acting constructor"
        )
    }
}

/// Round trip: `fromJSON . toJSON` recovers every constructor of a
/// three-way enum — the acceptance shape mirroring `Harness.hs`'s `Mode`.
#[test]
fn round_trip_enum_all_constructors() {
    let src = format!(
        "{HEADER}\n\
         data Mode = Observing | Deciding | Acting deriving (Generic, ToJSON, FromJSON, Eq)\n\n\
         roundTrips :: Mode -> Bool\n\
         roundTrips m = case (fromJSON (toJSON m) :: Result Mode) of\n\
         \x20 Success m' -> m' == m\n\
         \x20 Error _ -> False\n\n\
         result :: Bool\n\
         result = roundTrips Observing && roundTrips Deciding && roundTrips Acting\n"
    );
    if let Some(v) = run(&src, "result") {
        assert_eq!(
            v,
            json!(true),
            "every constructor round-trips through toJSON/fromJSON"
        )
    }
}

/// An unknown string is a decode `Error`, not a crash — the enum decoder's
/// failure path returns a value rather than trapping.
#[test]
fn unknown_string_returns_error() {
    let src = format!(
        "{HEADER}\n\
         data Mode = Observing | Deciding | Acting deriving (Generic, FromJSON)\n\n\
         result :: Int\n\
         result = case (fromJSON (String \"Sleeping\") :: Result Mode) of\n\
         \x20 Success _ -> 1\n\
         \x20 Error _ -> 0\n"
    );
    if let Some(v) = run(&src, "result") {
        assert_eq!(
            v,
            json!(0),
            "unrecognized constructor name decodes to Error (0), not a crash"
        )
    }
}

/// A sum mixing a nullary and a non-nullary constructor is still rejected at
/// COMPILE time — only all-nullary sums (enums) derive; a partial sum is out
/// of scope, same as a fully non-nullary sum.
#[test]
fn mixed_nullary_sum_still_rejected_at_compile_time() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract unavailable");
        return;
    }
    let src = format!(
        "{HEADER}\n\
         data M = A | B Int deriving (Generic, FromJSON)\n\n\
         result :: Int\n\
         result = 0\n"
    );
    match EvalHarness::new().with_stdlib().compile(&src, "result") {
        Ok(_) => panic!("mixed nullary/non-nullary sum deriving FromJSON must not compile"),
        Err(e) => {
            let msg = tidepool_runtime::classify_compile(&e).message;
            assert!(
                msg.contains("single-constructor records only"),
                "expected the generic sum-rejection TypeError, got:\n{msg}"
            );
        }
    }
}
