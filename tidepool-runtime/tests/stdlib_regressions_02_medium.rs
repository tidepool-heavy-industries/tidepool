//! Regression tests for stdlib findings M1-M4, M6-M9, and a selection of the
//! LOW list (H1-H4/M5/M10 are covered by `stdlib_regressions_02.rs` — not
//! duplicated here).
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
//! like upstream lens-aeson, instead of truncating toward zero — fixed via
//! `Scientific.hs`'s `integerValue` switching its negative-exponent branch
//! from `quot` (truncates toward zero) to `div` (floors), and renaming the
//! now-misleadingly-named `truncateScientific`/`truncateBoundedInteger` to
//! `floorScientific`/`floorBoundedInteger`.
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
//!   exactly 8).
//! - `app/Main.hs`'s DataCon meta keying (`(dcid, qname)` instead of `dcid`
//!   alone) — only exercised by the `--all-closed` multi-target fixture
//!   generation path in the extractor binary itself, not by any single JIT
//!   eval; verified by inspection that the new keying matches
//!   `tsUsedDCs`/`mergeMetaPreserving`'s existing `(varId, qname)` convention
//!   (`src/Tidepool/Translate.hs`).
//!
//! Most of the above findings bundle into three `#[test]` fns via the
//! check-list idiom (`generic_form_roundtrip.rs`/`jit_surface.rs`), grouped
//! by theme AND by import needs (the `[fmt|...|]` checks need the
//! conditional `Tidepool.QQ` import `eval_raw` already injects via
//! `uses_qq`, so they get their own bundle rather than forcing that import
//! on every other check). Eight probes stay standalone — see each one's
//! comment for why (`works_int_prism_floors_not_truncates`, kept standalone
//! rather than folded into its family bundle now that it's fixed; two
//! deliberate-failure assertions; one crash-class regression; and the four
//! `Patch.*` checks, which are
//! merely un-bundled rather than unbundlable: see the SAFE TO BUNDLE note
//! above `works_patch_tab_timestamp_not_treated_as_rename`) — and the
//! commit message carries the full value-vs-crash sort.

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
    let include = [
        hs.as_path(),
        lib.as_path(),
        effects_dir.core.as_path(),
        effects_dir.shim.as_path(),
    ];
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
// M1 (non-finite Double -> Null) + M2 (bounds-checked Int decode/prism,
// plus the _Integer floor sibling case) + LOW (parseDoubleM e-notation/
// overflow, addUTCTime rounding). VALUE-class throughout: every regression here is a
// wrong-but-obtained value from a successfully-compiled, successfully-run
// probe — none of these mechanisms crash or hang on regression.
// =========================================================================

/// Absorbed: works_tojson_double_infinity_is_null,
/// works_tojson_double_nan_is_null, works_tojson_double_finite_unaffected,
/// works_eitherdecode_int_out_of_range_is_left,
/// works_eitherdecode_int_in_range_unaffected,
/// works_int_prism_out_of_range_is_nothing,
/// works_parsedoublem_accepts_exponent_notation,
/// works_parsedoublem_roundtrips_showdouble_extreme_magnitudes,
/// works_addutctime_rounds_ms_conversion.
///
/// `integer_prism_floors_negative_fractional` below is the sibling-audit
/// addition for the `works_int_prism_floors_not_truncates` fix (`_Int`'s
/// unbounded cousin `_Integer` had the exact same truncate-toward-zero bug —
/// see `stdlib_regressions_02_medium.rs`'s module doc, M2/LOW).
///
/// `works_int_prism_floors_not_truncates` itself stays standalone below,
/// unchanged in shape — the fix makes it green; moving it into this bundle
/// too is unnecessary churn now that its sibling case lives here.
#[test]
fn works_numeric_json_and_parsing_family() {
    works(
        r#"pure (concat
            [ check "tojson_double_infinity_is_null"
                -- M1: pre-fix, 1/0 rendered as a garbage finite number from
                -- fromDouble parsing the LETTERS of "Infinity" as decimal
                -- digits; must be JSON Null (upstream aeson parity).
                (toJSON (1/0 :: Double) == Null)
            , check "tojson_double_nan_is_null"
                -- M1: same for 0/0 (NaN).
                (toJSON (0/0 :: Double) == Null)
            , check "tojson_double_finite_unaffected"
                -- M1 control: the isFiniteDouble guard must not touch finite
                -- Doubles.
                ((toJSON (2.5 :: Double) ^? _Double) == Just 2.5)
            , check "eitherdecode_int_out_of_range_is_left"
                -- M2: pre-fix silently wrapped to Right (-1) instead of a
                -- bounds-checked Left (toBoundedInteger).
                (case (eitherDecode "18446744073709551615" :: Either Text Int) of { Right _ -> False; Left _ -> True })
            , check "eitherdecode_int_in_range_unaffected"
                -- M2 control: in-range decode unaffected by the bounds
                -- check.
                ((case (eitherDecode "42" :: Either Text Int) of { Right n -> n; Left _ -> -999 }) == 42)
            , check "int_prism_out_of_range_is_nothing"
                -- M2: the _Int prism (Aeson/Lens.hs) bounds-checks like
                -- upstream lens-aeson; out-of-range must be Nothing.
                (not (isJust ((toJSON (1.0e30 :: Double)) ^? _Int)))
            , check "integer_prism_floors_negative_fractional"
                -- Sibling-audit addition for the _Int floor fix
                -- (works_int_prism_floors_not_truncates): _Integer had the
                -- same truncate-toward-zero bug on a negative fractional
                -- Double; upstream lens-aeson floors ("-3.7" -> -4).
                ((toJSON (-3.7 :: Double) ^? _Integer) == Just (-4))
            , check "parsedoublem_accepts_exponent_notation.e1"
                -- LOW: parseDoubleM accepts e-notation.
                ((case parseDoubleM "1.25e1" of { Just dA -> dA; Nothing -> -1 }) == 12.5)
            , check "parsedoublem_accepts_exponent_notation.eneg2"
                ((case parseDoubleM "1e-2" of { Just dB -> dB; Nothing -> -1 }) == 0.01)
            , check "parsedoublem_roundtrips_extreme.e19"
                -- LOW: digits accumulate as a Double (no Int overflow past
                -- ~19 digits) — round-trips showT at extreme magnitudes.
                (case parseDoubleM (showT (1.0e19 :: Double)) of { Just dC -> dC == (1.0e19 :: Double); Nothing -> False })
            , check "parsedoublem_roundtrips_extreme.eneg10"
                (case parseDoubleM (showT (1.5e-10 :: Double)) of { Just dD -> dD == (1.5e-10 :: Double); Nothing -> False })
            , check "addutctime_rounds_ms_conversion"
                -- LOW: addUTCTime rounds (not truncates) the
                -- seconds->milliseconds conversion, so diffUTCTime
                -- round-trips 1.005s exactly.
                ((let t0 = UTCTime 0 in diffUTCTime (addUTCTime 1.005 t0) t0) == 1.005)
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

// =========================================================================
// M3 (diff -u timestamp headers) + M4 (no-newline markers, both
// directions). Still one-compile-per-test — but now only because nobody has
// bundled them yet, NOT because bundling is unsafe. See below.
// =========================================================================

// SAFE TO BUNDLE. These four were held standalone because bundling them hit
// a reproducible wrong answer that depended on the ORDER of the checks in
// the `concat`. That bug is root-caused and fixed: it was never about
// `Patch.hs` or about co-residence. `Tidepool/Translate.hs` passed GHC's
// `dataToTag#` straight through to a `DataToTag` primop whose backends
// answer with the runtime constructor tag — a stableVarId hash of the
// constructor's NAME — where GHC's contract is the constructor's 0-based
// index within its own data type, so `fpCreate fp == True` compared a hash
// against `1#` and lost. Order only decided whether GHC emitted the primop
// at all (it does so when the comparison feeds a shared `Int#` join point,
// which the SECOND check is what creates).
//
// `tidepool-runtime/tests/patch_crosscheck_differential.rs` carries the
// mechanism, the both-engines pin, and a structural guard that no
// `DataToTag` primop survives translation. Bundling these four into one
// `works_patch_family` is now an ordinary compile-cost win, and the
// oracle-vs-JIT matrix over exactly these checks is already green.

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
// M6 (Slice clamping) + M7 (camelToSnake) + M8 (Unicode char classes) + M9
// (center padding) + LOW (nubBy argument order, Tab.parseCsv quoting).
// VALUE-class throughout.
// =========================================================================

/// Absorbed: works_slice_list_stake_negative_clamps_to_empty,
/// works_slice_list_sdrop_negative_clamps_to_whole,
/// works_cameltosnake_no_leading_underscore, works_isalpha_unicode_letter,
/// works_isspace_unicode_nbsp, works_center_pads_odd_char_left,
/// works_textformat_centerwith_agrees, works_nubby_argument_order_matches_base,
/// works_parsecsv_quoted_field_with_embedded_comma.
#[test]
fn works_text_slice_and_csv_family() {
    works(
        r#"pure (concat
            [ check "slice_list_stake_negative_clamps_to_empty"
                -- M6: Slice [a]'s stake/sdrop now clamp n<=0 like base
                -- take/drop and the Slice Text instance.
                (stake (-1) [1,2,3::Int] == [])
            , check "slice_list_sdrop_negative_clamps_to_whole"
                (sdrop (-1) [1,2,3::Int] == [1,2,3])
            , check "cameltosnake_no_leading_underscore"
                -- M7: TF.camelToSnake no longer emits a leading underscore
                -- for PascalCase input.
                (TF.camelToSnake "HelloWorld" == "hello_world")
            , check "isalpha_unicode_letter"
                -- M8: isAlpha/isUpper/isSpace now match Data.Char's Unicode
                -- semantics instead of an ASCII-only range check. 'é'
                -- (U+00E9) is alphabetic under Unicode but outside the old
                -- ASCII a-z/A-Z range.
                (isAlpha '\233' == True)
            , check "isspace_unicode_nbsp"
                -- M8: U+00A0 (NO-BREAK SPACE) is whitespace under Unicode
                -- (category Zs) but outside the old ASCII whitespace set.
                (isSpace '\160' == True)
            , check "center_pads_odd_char_left"
                -- M9: center/TF.centerWith pad the ODD leftover character on
                -- the LEFT (matching their own haddock), not the right.
                (center 10 '-' "hello" == "---hello--")
            , check "textformat_centerwith_agrees"
                (TF.centerWith 10 '-' "hello" == "---hello--")
            , check "nubby_argument_order_matches_base"
                -- LOW: nubBy's predicate argument order now matches base
                -- (kept-element first): eq keptItem candidate.
                (nubBy (\a b -> a > b) [3,1,4,1,5,9,2,6::Int] == [3,4,5,9])
            , check "parsecsv_quoted_field_with_embedded_comma"
                -- LOW: Tab.parseCsv is now quote-aware (RFC-4180): a quoted
                -- field may contain the delimiter.
                (Tab.parseCsv "a,\"b,c\",d" == [["a", "b,c", "d"]])
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

// =========================================================================
// LOW — `[fmt|...|]` digit grouping (size 4, not 3, for hex/octal/binary)
// and `fmtFrac`'s overflow-past-2^63 fix. Own bundle: these need the
// conditional `Tidepool.QQ` import `eval_raw` injects via `uses_qq`, so
// keeping them separate avoids forcing that import onto every other check
// in this file (rule 4: group by compile compatibility).
// =========================================================================

/// Absorbed: works_fmt_hex_grouping_is_four_not_three,
/// works_fmtfrac_beyond_2_63_does_not_saturate.
///
/// NOT absorbed: `fails_fmt_hex_comma_grouping_rejected` is a
/// compile/runtime-FAIL assertion (structurally incompatible with a bundle
/// whose eval must SUCCEED to return a check list) — stays standalone below.
#[test]
fn works_fmt_quoter_family() {
    works(
        r#"pure (concat
            [ check "fmt_hex_grouping_is_four_not_three"
                -- 4886718345 == 0x123456789 (9 hex digits): grouped by 4
                -- from the right -> "1_2345_6789" (grouping by 3 would give
                -- "123_456_789").
                ([fmt|{hexGroupN:_x}|] == "1_2345_6789")
            , check "fmtfrac_beyond_2_63_does_not_saturate"
                -- fmtFrac's {1.0e19:.0f} no longer saturates to garbage past
                -- 2^63 (matches Rust's own correctly-rounded f64->decimal
                -- {:.0}, computed once at the value's exact IEEE-754 bit
                -- pattern — round is a no-op at this magnitude).
                ([fmt|{bigFracN:.0f}|] == "10000000000000000000")
            ])
         where
           { check nm ok = if ok then [] else [nm]
           ; hexGroupN = 4886718345 :: Int
           ; bigFracN = 1.0e19 :: Double
           }"#,
        serde_json::json!([]),
    );
}

// =========================================================================
// Standalone probes — the value-vs-crash sort (see also the commit message).
// =========================================================================

/// `_Int` floors toward negative infinity for a fractional Double, matching
/// upstream lens-aeson (fixed in `Scientific.hs`'s `integerValue`: the
/// negative-exponent branch now uses `div`, not `quot`). See
/// `integer_prism_floors_negative_fractional` above for the `_Integer`
/// sibling case.
#[test]
fn works_int_prism_floors_not_truncates() {
    // lens-aeson: "-3.7" floors to -4 (truncation toward zero would give -3).
    works(
        r#"pure (case (toJSON (-3.7 :: Double) ^? _Int) of { Just n -> n; Nothing -> -999 })"#,
        serde_json::json!(-4),
    );
}

/// COMPILE-FAIL assertion — STANDALONE: asserts the probe itself fails, so
/// it cannot share an eval whose contract is "succeeds and returns a check
/// list".
#[test]
fn fails_fmt_hex_comma_grouping_rejected() {
    fails(
        r#"pure ([fmt|{n:,x}|]) where { n = 255 :: Int }"#,
        "Cannot specify ','",
    );
}

/// CRASH-class — STANDALONE: `Runtime.hs`'s own comment names this
/// literally — negating `Int` `minBound` overflows, which "would otherwise
/// crash `digitsInBase` on `tbl !! r` with a negative index". A regression
/// here is `Prelude.!!: negative index`-shaped, not a wrong value — bundling
/// it risks taking out sibling checks' diagnosis if it recurs.
#[test]
fn works_fmtint_minbound_does_not_crash() {
    works(
        r#"pure ([fmt|{n:d}|]) where { n = minBound :: Int }"#,
        serde_json::json!("-9223372036854775808"),
    );
}

/// COMPILE-FAIL assertion — STANDALONE, same reasoning as
/// `fails_fmt_hex_comma_grouping_rejected`.
#[test]
fn fails_formatiso8601_negative_year_errors_loudly() {
    fails(
        r#"pure (show (UTCTime (-100000000000000)))"#,
        "negative year",
    );
}
