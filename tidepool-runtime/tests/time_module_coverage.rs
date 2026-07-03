//! Golden round-trip tests for Tidepool.Data.Time: parseISO8601 / formatISO8601.
//!
//! Covers:
//!   - format→parse round-trip (epochMillis identity)
//!   - parse→format round-trip (Z-suffix string identity)
//!   - pre-1970 dates (negative epoch-ms)
//!   - git %cI shape with negative UTC offset (e.g. -07:00)
//!   - git %cI shape with positive UTC offset (e.g. +05:30)
//!
//! All functions are pure-Int JIT-safe (no FFI, no Integer, no lens).

use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

fn run(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
default (Int, Text)

result :: _
result = {body}
"#
    );
    EvalHarness::new()
        .with_stdlib()
        .run_pure(&src, "result")
        .expect("compile_and_run_pure failed")
        .to_json()
}

// Golden: format→parse round-trip — modern timestamp (2024-02-29 leap day).
// epochMillis (parseISO8601 (formatISO8601 t)) == epochMillis t
#[test]
fn test_format_then_parse_modern() {
    assert_eq!(
        run("epochMillis (parseISO8601 (formatISO8601 (UTCTime 1709164800000)))"),
        json!(1709164800000i64)
    );
}

// Golden: parse→format round-trip — Z-suffix form stays identical.
// formatISO8601 (parseISO8601 s) == s
#[test]
fn test_parse_then_format_z() {
    assert_eq!(
        run(r#"formatISO8601 (parseISO8601 "2024-02-29T00:00:00Z")"#),
        json!("2024-02-29T00:00:00Z")
    );
}

// Golden: pre-1970 date round-trip — negative epoch milliseconds.
// 1960-03-15T12:00:00Z is ~10 years before Unix epoch.
#[test]
fn test_roundtrip_pre1970() {
    assert_eq!(
        run(r#"formatISO8601 (parseISO8601 "1960-03-15T12:00:00Z")"#),
        json!("1960-03-15T12:00:00Z")
    );
}

// Golden: epoch milliseconds for a pre-1970 timestamp are negative.
#[test]
fn test_pre1970_epoch_millis_negative() {
    // 1960-03-15T12:00:00Z = -309182400000 ms
    assert_eq!(
        run(r#"epochMillis (parseISO8601 "1960-03-15T12:00:00Z")"#),
        json!(-309182400000i64)
    );
}

// Golden: git %cI format with negative UTC offset.
// "2026-07-01T19:24:22-07:00" normalises to "2026-07-02T02:24:22Z" (UTC).
#[test]
fn test_parse_git_ci_negative_offset() {
    assert_eq!(
        run(r#"formatISO8601 (parseISO8601 "2026-07-01T19:24:22-07:00")"#),
        json!("2026-07-02T02:24:22Z")
    );
}

// Golden: git %cI format with positive UTC offset.
// "2024-01-15T10:30:00+05:30" normalises to "2024-01-15T05:00:00Z" (IST→UTC).
#[test]
fn test_parse_git_ci_positive_offset() {
    assert_eq!(
        run(r#"formatISO8601 (parseISO8601 "2024-01-15T10:30:00+05:30")"#),
        json!("2024-01-15T05:00:00Z")
    );
}

// Golden: daysFromCivil is the exact inverse of civilFromDays.
// formatISO8601 (UTCTime 0) == "1970-01-01T00:00:00Z"; parse gives back 0.
#[test]
fn test_epoch_zero_roundtrip() {
    assert_eq!(
        run("epochMillis (parseISO8601 (formatISO8601 (UTCTime 0)))"),
        json!(0i64)
    );
}
