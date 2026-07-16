//! Regression tests for repo-review-2026-07-06/02-haskell-stdlib.md findings
//! H1, H2, H3, H4 (M5 and M10 are covered separately — see below).
//!
//! H1/H2 — non-ASCII `String` literals were silently corrupted: GHC embeds a
//! String literal's Addr# as raw UTF-8 bytes, but the extractor's static
//! `unpackCString#`/`unpackFoldrCString#` arms emitted one `LEChar` cons cell
//! PER BYTE instead of per Unicode code point (`map fromEnum "hé"` gave
//! `[104,195,169]` instead of `[104,233]`). H2's fusion-context abort
//! (`pure (T.length "héllo")` failing with "Dangling NVar:
//! unpackFoldrCStringUtf8#") turned out to share more than the byte-decode
//! bug: `T.length`'s foldr/build fusion instantiates the `a` tyvar as
//! `Int -> Int` (the length-accumulator trick), so the fully-fused
//! application carries a FOURTH value arg beyond the syntactic
//! `(lit, f, z)` triple — an exact-length `[litArg, fArg, zArg]` list
//! pattern never matches, so translation fell through to a dangling `NVar`.
//! Fixed in `Translate.hs` by (a) a shared `utf8CodepointsOf` decode helper
//! applied at every literal-unpack cons-cell site, and (b) generalizing the
//! `unpackFoldrCString#` static arm to `(litArg:fArg:zArg:extraArgs)`,
//! re-applying any trailing args to the expanded result.
//!
//! H3 — `replicate` with a negative count never terminated (`go` had no
//! base case below 0). H4 — `splitAt` with a negative count returned its
//! components exactly swapped (`(xs, [])` instead of base's `([], xs)`).
//!
//! M5/M10 are not JIT-eval-reachable (M5) or don't need a fresh test harness
//! shape (M10 shares `works_len_class` coverage) — see the dedicated notes
//! at the bottom of this file for how each is actually pinned.

use std::path::Path;
use std::time::Duration;
use tidepool_testing::NullDispatcher;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

/// Compile `code` (a single Haskell expression of type `M a`) under the full
/// MCP preamble and run it. Mirrors `jit_surface.rs::eval_raw`.
fn eval_raw(code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, "", "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [hs.as_path(), lib.as_path(), effects_dir.as_path()];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(format!("{e}")),
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

// =========================================================================
// H1 — non-ASCII String literals must decode as Unicode code points, not
// raw UTF-8 bytes. Covers a 2-byte (é, U+00E9), a 3-byte BMP char (—,
// U+2014), and a 3-byte currency symbol (€, U+20AC) — root's requested
// spread beyond the single-accent repro.
// =========================================================================

#[test]
fn works_nonascii_string_literal_fromenum() {
    // Pre-fix this returned [104,195,169,226,128,148,226,130,172] (one LEChar
    // per UTF-8 BYTE of "hé—€"): h, then é's two bytes, then —'s three bytes,
    // then €'s three bytes — 9 entries instead of 4 code points.
    works(
        r#"pure (map fromEnum ("h\233\8212\8364" :: String))"#,
        serde_json::json!([104, 233, 8212, 8364]),
    );
}

#[test]
fn works_nonascii_string_literal_direct_utf8_source() {
    // Same repro, but with the actual UTF-8-encoded source characters (not
    // \NNNN escapes) — proves the fix holds regardless of how GHC's lexer
    // got to the same Addr# literal bytes.
    works(
        "pure (map fromEnum (\"h\u{e9}\u{2014}\u{20ac}\" :: String))",
        serde_json::json!([104, 233, 8212, 8364]),
    );
}

#[test]
fn works_nonascii_text_pack_roundtrip() {
    // Pre-fix: T.pack "hé" rendered as "hÃ©" mojibake (each byte reinterpreted
    // as a separate Latin-1-ish code point, then re-encoded to UTF-8 on the
    // way out).
    works(
        r#"pure (T.pack "h\233\8212\8364")"#,
        serde_json::json!("h\u{e9}\u{2014}\u{20ac}"),
    );
}

// =========================================================================
// H2 — fusion-context (build/foldr) non-ASCII literals must not abort
// extraction. `T.length` instantiates the foldr accumulator as `Int -> Int`,
// which is what actually broke the exact-arity arg match (see module doc).
// =========================================================================

#[test]
fn works_nonascii_fusion_context_length() {
    // Pre-fix: aborted extraction with "Dangling NVar:
    // unpackFoldrCStringUtf8#... This is an extract-pipeline bug... not a
    // user error" — the ASCII equivalent (T.length (T.pack "hello")) worked
    // fine, isolating the bug to non-ASCII fusion.
    works(
        r#"pure (T.length (T.pack "h\233llo"))"#,
        serde_json::json!(5),
    );
}

// =========================================================================
// H3 — `replicate` with a negative count must return [] immediately, not
// hang. Run on a background thread with a short recv_timeout: pre-fix this
// call never returns, so a timeout (rather than the expected value) is the
// observable "still broken" signal.
// =========================================================================

#[test]
fn works_replicate_negative_n_terminates() {
    let (tx, rx) = std::sync::mpsc::channel::<Result<serde_json::Value, String>>();
    std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            let _ = tx.send(eval_raw("pure (replicate (2 - 5) 'x' :: String)"));
        })
        .unwrap();
    // 10s was too tight: a cold per-eval GHC compile alone regularly takes
    // 20-70s in this suite (see e.g. works_decode/works_either_decode above),
    // so a short window reads as "still hanging" for ANY eval, fixed or not.
    // 120s comfortably covers a cold compile with room to spare.
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(v)) => assert_eq!(
            v,
            serde_json::json!(""),
            "replicate with a negative count must be the empty list"
        ),
        Ok(Err(e)) => panic!("replicate (negative) failed instead of returning []: {e}"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
            "replicate (negative n) is STILL HANGING (H3 not fixed) — \
             `go` must have a `m <= 0` base case"
        ),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("eval thread panicked before sending a result")
        }
    }
}

// =========================================================================
// H4 — `splitAt` with a negative count must match base: ([], xs), not the
// components swapped.
// =========================================================================

#[test]
fn works_splitat_negative_n_matches_base() {
    works(
        "pure (splitAt (2 - 5) [1,2,3::Int])",
        serde_json::json!([[], [1, 2, 3]]),
    );
}

// =========================================================================
// M5 — NOT covered here. The 2-result unboxed-tuple fallback landmine
// (Translate.hs, the generic `case op ... of (# ... #)` arms) has no
// reachable path from any exported stdlib function — every primop the
// stdlib actually calls that returns a genuine multi-result unboxed tuple
// (quotRem, addC/subC, decodeDouble_Int64#, ...) already has a dedicated
// split in `splitMultiReturnPrimOp`/`splitUnaryMultiReturnPrimOp`, so no
// JIT-level Haskell expression reaches the buggy fallback arm at all — that
// unreachability is exactly the "landmine" M5 describes. The fix (a named,
// loud `error` at extract time instead of silently aliasing both result
// binders to one primop node) was verified directly against the extract
// binary with hand-written `MagicHash`/`UnboxedTuples` source hitting the
// two now-error'd arms:
//   - pure:     `case decodeFloat_Int# x of (# m, e #) -> ...`
//     -> "Unsupported 2-result pure unboxed-tuple primop: decodeFloat_Int#"
//   - stateful: `case casArray# arr# i old new s of (# s', flag, oldVal #) -> ...`
//     -> "Unsupported 2-result stateful unboxed-tuple primop/FFI call: casArray#"
// Both now abort extraction with a named marker (SKIPPED, not a crash) —
// this can't be expressed as a `code: &str` JIT probe (the MCP preamble's
// pragmas are fixed; MagicHash/UnboxedTuples aren't in it and can't be
// added per-eval), so it isn't duplicated as a Rust test here.

// =========================================================================
// M10 — `Len [a]` non-guarded recursion (`1 + len xs`) must not blow the
// JIT stack on long lists; fixed to the same accumulator shape as `length`.
// =========================================================================

#[test]
fn works_len_class_long_list_no_stack_death() {
    // The repo's own evidence (Data/Text.hs) puts the JIT's non-tail-call
    // stack ceiling around ~20k frames; 50k comfortably exercises the fix
    // without depending on the exact ceiling.
    works(
        "pure (len (enumFromTo 1 50000 :: [Int]))",
        serde_json::json!(50000),
    );
}
