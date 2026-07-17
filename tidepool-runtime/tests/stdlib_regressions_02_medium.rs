//! Regression tests for repo-review-2026-07-06/02-haskell-stdlib.md findings
//! M1-M4, M6-M9, and a selection of the LOW list (H1-H4/M5/M10 are covered by
//! `stdlib_regressions_02.rs`, written by the previous wave — not duplicated
//! here).
//!
//! M1 — `toJSON` on a non-finite `Double` (`1/0`, `0/0`) must encode as JSON
//! `Null` (upstream aeson parity), not the garbage finite number that came
//! from `fromDouble` parsing the LETTERS of `"Infinity"`/`"NaN"` as decimal
//! digits. Fixed via a new `isFiniteDouble` guard (`Aeson/Scientific.hs`,
//! reusing `fmtFrac`'s NaN/Inf test) applied at the `ToJSON Double`/`Float`
//! call sites (`Aeson/Value.hs`) — `fromDouble` itself is unchanged (still
//! only valid on finite input) since it cannot express `Null`.
//!
//! M2 — `eitherDecode "18446744073709551615" :: Either Text Int` silently
//! wrapped to `Right (-1)`; now bounds-checked via the (previously
//! effectively-dead-for-this-path) `toBoundedInteger` and returns `Left`.
//! The `_Int`/`_Integer` prisms (`Aeson/Lens.hs`) get the same bounds check,
//! plus the LOW fix: they now FLOOR fractional numbers (`"-3.7"` -> `-4`)
//! like upstream lens-aeson, instead of truncating toward zero.
//!
//! M3 — `Patch.parsePatch`: a standard `diff -u` timestamp header
//! (`--- path\t<timestamp>`) no longer corrupts the path (tab+timestamp are
//! now truncated before comparison), so a plain in-place edit no longer
//! spuriously reads as an unsupported rename, and `/dev/null`
//! creation-detection still fires with a timestamp suffix.
//!
//! M4 — `\ No newline at end of file` markers are now tracked per side
//! (`Hunk`'s new `hOldNoNewline`/`hNewNoNewline` fields) and APPLIED: a hunk
//! that changes whether the file ends with a trailing newline now produces
//! exactly the right trailing newline in `applyFilePatch`'s output, in both
//! directions (gaining and losing the trailing newline).
//!
//! M6 — `Slice [a]`'s `stake`/`sdrop` now clamp `n <= 0` like base
//! `take`/`drop` and the `Slice Text` instance (`stake (-1) xs == []`).
//!
//! M7 — `TF.camelToSnake` no longer emits a leading underscore for
//! PascalCase input (`"HelloWorld"` -> `"hello_world"`, matching its own
//! doctest).
//!
//! M8 — `isAlpha`/`isUpper`/`isSpace` etc. now match `Data.Char`'s Unicode
//! semantics (delegate to `Data.Char`) instead of an ASCII-only range check.
//!
//! M9 — `center`/`TF.centerWith` now pad the ODD leftover character on the
//! LEFT (matching their own haddock), not the right.
//!
//! LOW — `parseDoubleM` accepts e-notation and accumulates digits as a
//! `Double` (no `Int` overflow past ~19 digits); `nubBy`'s predicate
//! argument order now matches base (kept-element first); `[fmt|{n:_x}|]`
//! groups hex/octal/binary by 4 (not 3) and rejects `,` grouping for them;
//! `fmtInt minBound` no longer crashes; `fmtFrac`'s `{1.0e19:.0f}` no longer
//! saturates to garbage; `addUTCTime` rounds (not truncates) the
//! seconds->milliseconds conversion; a negative civil year now errors loudly
//! in `formatISO8601` instead of rendering garbage digits; `Tab.parseCsv` is
//! now quote-aware (RFC-4180).
//!
//! NOT covered here (not JIT-eval-reachable, per M5's precedent):
//! - `CborEncode.hs`'s stale arity-5/6/7 comment — a comment fix, verified by
//!   direct inspection against `tidepool-repr/src/serial/read.rs` (requires
//!   exactly 7).
//! - `app/Main.hs`'s DataCon meta keying (`(dcid, qname)` instead of `dcid`
//!   alone) — only exercised by the `--all-closed` multi-target fixture
//!   generation path in the extractor binary itself, not by any single JIT
//!   eval; verified by inspection that the new keying matches
//!   `tsUsedDCs`/`mergeMetaPreserving`'s existing `(varId, qname)` convention
//!   (`src/Tidepool/Translate.hs`).

use std::path::Path;
use tidepool_runtime::compile_and_run;
use tidepool_testing::NullDispatcher;

/// Compile `code` (a single Haskell expression of type `M a`) under the full
/// MCP preamble and run it. Mirrors `jit_surface.rs::eval_raw`, plus the
/// real server's QQ-import injection (`lib.rs`'s `eval()`): `jit_surface.rs`
/// never exercises `[fmt|...|]`/`[patch|...|]` through `eval_raw`, so it gets
/// away with a permanently-empty `imports` string — this file's fmt-hole
/// probes need the conditional `Tidepool.QQ` import or `fmt` is unbound.
fn eval_raw(code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let imports = if tidepool_mcp::uses_qq(code) {
        "Tidepool.QQ (fmt, j, patch, uri)"
    } else {
        ""
    };
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, imports, "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [hs.as_path(), lib.as_path(), effects_dir.as_path()];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        // `Display` on `RuntimeError`/`CompileError::Diagnostics` is a terse
        // structural summary now (structured spans, not rendered text) — the
        // classifier's message is the joined diagnostic text these probes
        // actually assert markers against.
        Err(e) => Err(tidepool_runtime::classify(&e).message),
    }
}

/// Run a probe on the shared `EVAL_STACK_SIZE` with signal safety installed,
/// same as `jit_surface.rs::run_probe`.
fn run_probe(code: &str) -> Result<serde_json::Value, String> {
    let code = code.to_string();
    std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            eval_raw(&code)
        })
        .unwrap()
        .join()
        .map_err(|_| {
            "thread panicked (HARD crash / uncaught signal — possible STILL-SILENT footgun)"
                .to_string()
        })?
}

fn works(code: &str, expected: serde_json::Value) {
    match run_probe(code) {
        Ok(got) => assert_eq!(
            got, expected,
            "\nprobe returned the wrong value:\n  code: {code}\n  want: {expected}\n  got:  {got}"
        ),
        Err(e) => panic!("\nprobe FAILED (expected success):\n  code: {code}\n  error: {e}"),
    }
}

/// Assert a probe FAILS and the error text contains `marker` (mirrors
/// `jit_surface.rs::fails_loudly`).
fn fails(code: &str, marker: &str) {
    match run_probe(code) {
        Ok(v) => panic!(
            "\nprobe unexpectedly SUCCEEDED:\n  code: {code}\n  got:  {v}\n  \
             (expected a loud failure containing {marker:?})"
        ),
        Err(e) => assert!(
            e.contains(marker),
            "\nprobe failed but WITHOUT the expected marker:\n  code: {code}\n  \
             want marker: {marker:?}\n  error: {e}"
        ),
    }
}

// =========================================================================
// M1 — non-finite Doubles JSON-encode as Null, not garbage finite numbers.
// =========================================================================

#[test]
fn works_tojson_double_infinity_is_null() {
    works(r#"pure (toJSON (1/0 :: Double))"#, serde_json::Value::Null);
}

#[test]
fn works_tojson_double_nan_is_null() {
    works(r#"pure (toJSON (0/0 :: Double))"#, serde_json::Value::Null);
}

#[test]
fn works_tojson_double_finite_unaffected() {
    works(r#"pure (toJSON (2.5 :: Double))"#, serde_json::json!(2.5));
}

// =========================================================================
// M2 — out-of-range integer JSON decode is a typed failure, not a silent
// wraparound; `_Int`/`_Integer` floor fractional numbers and bounds-check.
// =========================================================================

#[test]
fn works_eitherdecode_int_out_of_range_is_left() {
    works(
        r#"pure (case (eitherDecode "18446744073709551615" :: Either Text Int) of { Right _ -> False; Left _ -> True })"#,
        serde_json::json!(true),
    );
}

#[test]
fn works_eitherdecode_int_in_range_unaffected() {
    works(
        r#"pure (case (eitherDecode "42" :: Either Text Int) of { Right n -> n; Left _ -> -999 })"#,
        serde_json::json!(42),
    );
}

#[test]
fn works_int_prism_floors_not_truncates() {
    // lens-aeson: "-3.7" floors to -4 (truncation toward zero would give -3).
    works(
        r#"pure (case (toJSON (-3.7 :: Double) ^? _Int) of { Just n -> n; Nothing -> -999 })"#,
        serde_json::json!(-4),
    );
}

#[test]
fn works_int_prism_out_of_range_is_nothing() {
    works(
        r#"pure (case (toJSON (1.0e30 :: Double) ^? _Int) of { Just _ -> False; Nothing -> True })"#,
        serde_json::json!(true),
    );
}

// =========================================================================
// M3 — a `diff -u` timestamp header (`path\t<timestamp>`) must not corrupt
// the path.
// =========================================================================

#[test]
fn works_patch_tab_timestamp_not_treated_as_rename() {
    works(
        r#"pure (case Patch.parsePatch (T.intercalate "\n" ["--- a/foo.txt\t2026-01-01 12:00:00.000000000 +0000", "+++ b/foo.txt\t2026-01-01 12:00:01.000000000 +0000", "@@ -1,1 +1,1 @@", "-old", "+new"]) of { Right [fp] -> Patch.fpPath fp; _ -> "PARSE-FAIL" })"#,
        serde_json::json!("foo.txt"),
    );
}

#[test]
fn works_patch_devnull_create_with_tab_timestamp() {
    works(
        r#"pure (case Patch.parsePatch (T.intercalate "\n" ["--- /dev/null\t1970-01-01 00:00:00.000000000 +0000", "+++ b/new.txt\t2026-01-01 00:00:00.000000000 +0000", "@@ -0,0 +1,1 @@", "+hello"]) of { Right [fp] -> Patch.fpCreate fp; _ -> False })"#,
        serde_json::json!(true),
    );
}

// =========================================================================
// M4 — a `\ No newline at end of file` marker must be applied, not
// discarded, in both directions (losing and gaining the trailing newline).
// =========================================================================

#[test]
fn works_patch_apply_marker_new_side_loses_trailing_newline() {
    // Original "line1\nline2\n" (trailing newline); the hunk's new side is
    // the SAME text but marked no-newline -> output must lose it.
    works(
        r#"pure (case Patch.parsePatch (T.intercalate "\n" ["--- a/f.txt", "+++ b/f.txt", "@@ -1,2 +1,2 @@", " line1", "-line2", "+line2", "\\ No newline at end of file"]) of { Right (fp:_) -> (case Patch.applyFilePatch fp (Just (T.intercalate "\n" ["line1", "line2", ""])) of { Right (out, _) -> out; Left _ -> "CONFLICT" }); Left _ -> "PARSE-FAIL" })"#,
        serde_json::json!("line1\nline2"),
    );
}

#[test]
fn works_patch_apply_marker_new_side_gains_trailing_newline() {
    // Original "line1\nline2" (no trailing newline); the old side is marked
    // no-newline, the new side is not -> output must GAIN a trailing newline.
    works(
        r#"pure (case Patch.parsePatch (T.intercalate "\n" ["--- a/f.txt", "+++ b/f.txt", "@@ -1,2 +1,2 @@", " line1", "-line2", "\\ No newline at end of file", "+line2"]) of { Right (fp:_) -> (case Patch.applyFilePatch fp (Just (T.intercalate "\n" ["line1", "line2"])) of { Right (out, _) -> out; Left _ -> "CONFLICT" }); Left _ -> "PARSE-FAIL" })"#,
        serde_json::json!("line1\nline2\n"),
    );
}

// =========================================================================
// M6 — `Slice [a]` clamps negative n like base take/drop.
// =========================================================================

#[test]
fn works_slice_list_stake_negative_clamps_to_empty() {
    works(r#"pure (stake (-1) [1,2,3::Int])"#, serde_json::json!([]));
}

#[test]
fn works_slice_list_sdrop_negative_clamps_to_whole() {
    works(
        r#"pure (sdrop (-1) [1,2,3::Int])"#,
        serde_json::json!([1, 2, 3]),
    );
}

// =========================================================================
// M7 — camelToSnake suppresses the leading underscore at position 0.
// =========================================================================

#[test]
fn works_cameltosnake_no_leading_underscore() {
    works(
        r#"pure (TF.camelToSnake "HelloWorld")"#,
        serde_json::json!("hello_world"),
    );
}

// =========================================================================
// M8 — isAlpha/isUpper/isSpace match Data.Char's Unicode semantics.
// =========================================================================

#[test]
fn works_isalpha_unicode_letter() {
    // 'é' (U+00E9) is alphabetic under Unicode but outside the old ASCII
    // a-z/A-Z range check.
    works(r#"pure (isAlpha '\233')"#, serde_json::json!(true));
}

#[test]
fn works_isspace_unicode_nbsp() {
    // U+00A0 (NO-BREAK SPACE) is whitespace under Unicode (category Zs) but
    // outside the old ASCII whitespace set.
    works(r#"pure (isSpace '\160')"#, serde_json::json!(true));
}

// =========================================================================
// M9 — center/TF.centerWith pad the odd leftover character on the LEFT.
// =========================================================================

#[test]
fn works_center_pads_odd_char_left() {
    works(
        r#"pure (center 10 '-' "hello")"#,
        serde_json::json!("---hello--"),
    );
}

#[test]
fn works_textformat_centerwith_agrees() {
    works(
        r#"pure (TF.centerWith 10 '-' "hello")"#,
        serde_json::json!("---hello--"),
    );
}

// =========================================================================
// LOW — parseDoubleM: e-notation + no Int overflow past ~19 digits.
// =========================================================================

#[test]
fn works_parsedoublem_accepts_exponent_notation() {
    works(
        // 1.25e1 (not a whole number, and exactly representable as a binary
        // fraction — 0.25 = 1/4 — so it avoids both the JSON int-vs-float
        // representation ambiguity a whole Double would introduce AND any
        // decimal-fraction rounding noise from the digit-by-digit summation).
        r#"pure (case parseDoubleM "1.25e1" of { Just d -> d; Nothing -> -1 })"#,
        serde_json::json!(12.5),
    );
    works(
        r#"pure (case parseDoubleM "1e-2" of { Just d -> d; Nothing -> -1 })"#,
        serde_json::json!(0.01),
    );
}

#[test]
fn works_parsedoublem_roundtrips_showdouble_extreme_magnitudes() {
    works(
        r#"pure (case parseDoubleM (showT (1.0e19 :: Double)) of { Just d -> d == (1.0e19 :: Double); Nothing -> False })"#,
        serde_json::json!(true),
    );
    works(
        r#"pure (case parseDoubleM (showT (1.5e-10 :: Double)) of { Just d -> d == (1.5e-10 :: Double); Nothing -> False })"#,
        serde_json::json!(true),
    );
}

// =========================================================================
// LOW — nubBy's predicate argument order matches base (kept element first).
// =========================================================================

#[test]
fn works_nubby_argument_order_matches_base() {
    // A directional (non-equivalence) predicate distinguishes argument
    // order: eq keptItem candidate = keptItem > candidate. Pre-fix (flipped
    // to eq candidate keptItem) this list nubs to [3,1,4,1]; base (and this
    // fix) give [3,4,5,9].
    works(
        r#"pure (nubBy (\a b -> a > b) [3,1,4,1,5,9,2,6::Int])"#,
        serde_json::json!([3, 4, 5, 9]),
    );
}

// =========================================================================
// LOW — [fmt|...|] digit grouping: size 4 (not 3) for hex/octal/binary, and
// ',' is rejected for them (Python semantics).
// =========================================================================

#[test]
fn works_fmt_hex_grouping_is_four_not_three() {
    // 4886718345 == 0x123456789 (9 hex digits): grouped by 4 from the right
    // -> "1_2345_6789" (grouping by 3 would give "123_456_789").
    works(
        r#"pure ([fmt|{n:_x}|]) where { n = 4886718345 :: Int }"#,
        serde_json::json!("1_2345_6789"),
    );
}

#[test]
fn fails_fmt_hex_comma_grouping_rejected() {
    fails(
        r#"pure ([fmt|{n:,x}|]) where { n = 255 :: Int }"#,
        "Cannot specify ','",
    );
}

// =========================================================================
// LOW — fmtInt minBound no longer crashes; fmtFrac no longer overflows past
// 2^63.
// =========================================================================

#[test]
fn works_fmtint_minbound_does_not_crash() {
    works(
        r#"pure ([fmt|{n:d}|]) where { n = minBound :: Int }"#,
        serde_json::json!("-9223372036854775808"),
    );
}

#[test]
fn works_fmtfrac_beyond_2_63_does_not_saturate() {
    // Compute the expected digit string the same way Rust's own
    // correctly-rounded f64->decimal formatting would (both GHC and Rust
    // parse the `1.0e19` literal to the identical IEEE-754 double, and at
    // this magnitude the value is already an exact integer, so `round` is a
    // no-op and the expected string is exactly Rust's `{:.0}` of that f64).
    let expected = format!("{:.0}", 1.0e19_f64);
    works(
        r#"pure ([fmt|{n:.0f}|]) where { n = 1.0e19 :: Double }"#,
        serde_json::Value::String(expected),
    );
}

// =========================================================================
// LOW — addUTCTime rounds (not truncates) the seconds->milliseconds
// conversion, so diffUTCTime round-trips it.
// =========================================================================

#[test]
fn works_addutctime_rounds_ms_conversion() {
    works(
        r#"pure (let t0 = UTCTime 0 in diffUTCTime (addUTCTime 1.005 t0) t0)"#,
        serde_json::json!(1.005),
    );
}

// =========================================================================
// LOW — a negative civil year errors loudly in formatISO8601 instead of
// rendering garbage digits.
// =========================================================================

#[test]
fn fails_formatiso8601_negative_year_errors_loudly() {
    fails(
        r#"pure (show (UTCTime (-100000000000000)))"#,
        "negative year",
    );
}

// =========================================================================
// LOW — Tab.parseCsv is quote-aware (RFC-4180): a quoted field may contain
// the delimiter.
// =========================================================================

#[test]
fn works_parsecsv_quoted_field_with_embedded_comma() {
    works(
        r#"pure (Tab.parseCsv "a,\"b,c\",d")"#,
        serde_json::json!([["a", "b,c", "d"]]),
    );
}
