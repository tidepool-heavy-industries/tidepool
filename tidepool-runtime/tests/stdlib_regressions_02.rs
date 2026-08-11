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
//!
//! H1's three checks and H4 bundle into two `#[test]` fns (one compile per
//! bundle instead of one per check) via the check-list idiom — see
//! `generic_form_roundtrip.rs`/`jit_surface.rs`. H3 and M10 stay standalone:
//! both are HANG/crash-class (a negative-count `replicate` that never
//! returns, a stack-depth probe), and a crash mid-probe would take out
//! whatever else shared that eval — see the commit message for the full
//! value-vs-crash sort.

use std::path::Path;
use std::time::Duration;
use tidepool_runtime::compile_and_run;
use tidepool_testing::NullDispatcher;

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
//
// VALUE-class: on regression each check gets a wrong-but-obtained value
// (never a crash/hang), so all three compose into one probe.
// =========================================================================

/// Absorbed: works_nonascii_string_literal_fromenum,
/// works_nonascii_string_literal_direct_utf8_source,
/// works_nonascii_text_pack_roundtrip.
#[test]
fn works_nonascii_string_literal_family() {
    works(
        r#"pure (concat
            [ check "nonascii_string_literal_fromenum"
                -- Pre-fix this returned [104,195,169,226,128,148,226,130,172]
                -- (one LEChar per UTF-8 BYTE of "hé—€"): h, then é's two
                -- bytes, then —'s three bytes, then €'s three bytes — 9
                -- entries instead of 4 code points.
                (map fromEnum ("h\233\8212\8364" :: String) == [104,233,8212,8364])
            , check "nonascii_string_literal_direct_utf8_source"
                -- Same repro, but with the actual UTF-8-encoded source
                -- characters (not \NNNN escapes) — proves the fix holds
                -- regardless of how GHC's lexer got to the same Addr#
                -- literal bytes.
                (map fromEnum ("hé—€" :: String) == [104,233,8212,8364])
            , check "nonascii_text_pack_roundtrip"
                -- Pre-fix: T.pack "hé" rendered as "hÃ©" mojibake (each byte
                -- reinterpreted as a separate Latin-1-ish code point, then
                -- re-encoded to UTF-8 on the way out).
                (T.pack "h\233\8212\8364" == "h\233\8212\8364")
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

// =========================================================================
// H2 — fusion-context (build/foldr) non-ASCII literals must not abort
// extraction. `T.length` instantiates the foldr accumulator as `Int -> Int`,
// which is what actually broke the exact-arity arg match (see module doc).
//
// H4 — `splitAt` with a negative count must match base: ([], xs), not the
// components swapped.
//
// VALUE-class: H2's regression is a clean, catchable extraction-abort
// Result (not a process crash); H4's is a wrong-but-obtained tuple. Neither
// mechanism risks corrupting the probe process, so they share one eval.
// =========================================================================

/// Absorbed: works_nonascii_fusion_context_length,
/// works_splitat_negative_n_matches_base.
#[test]
fn works_fusion_length_and_splitat_family() {
    works(
        r#"pure (concat
            [ check "nonascii_fusion_context_length"
                -- Pre-fix: aborted extraction with "Dangling NVar:
                -- unpackFoldrCStringUtf8#... This is an extract-pipeline
                -- bug... not a user error" — the ASCII equivalent
                -- (T.length (T.pack "hello")) worked fine, isolating the
                -- bug to non-ASCII fusion.
                (T.length (T.pack "h\233llo") == 5)
            , check "splitat_negative_n_matches_base"
                -- Pre-fix: components exactly swapped ((xs, []) instead of
                -- base's ([], xs)).
                (splitAt (2 - 5) [1,2,3::Int] == ([], [1,2,3]))
            ])
         where { check nm ok = if ok then [] else [nm] }"#,
        serde_json::json!([]),
    );
}

// =========================================================================
// H3 — `replicate` with a negative count must return [] immediately, not
// hang. Run on a background thread with a short recv_timeout: pre-fix this
// call never returns, so a timeout (rather than the expected value) is the
// observable "still broken" signal.
//
// CRASH(hang)-class — STANDALONE: the failure mode on regression is
// non-termination, not a wrong value. It already needs its own harness
// shape (a channel + bounded `recv_timeout`, since `run_probe`'s plain
// `.join()` would hang forever too) — bundling it with anything else means
// a regression hangs the whole bundle rather than failing fast.
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
// M5 — NOT covered here. The 2-result unboxed-tuple fallback landmine
// (Translate.hs, the generic `case op ... of (# ... #)` arms). Most primops
// the stdlib calls that return a genuine multi-result unboxed tuple
// (quotRem, addC/subC, decodeDouble_Int64#, ...) have a dedicated split in
// `splitMultiReturnPrimOp`/`splitUnaryMultiReturnPrimOp` and so never reach
// the fallback arm.
//
// `decodeFloat_Int#` DOES NOT, and it is reachable from exported stdlib:
// `eitherDecode "3.5" :: Either Text Float` routes through aeson's
// `parseRealFloat` and aborts extraction with the named error below. An
// earlier version of this comment claimed the fallback had no reachable path
// from any exported stdlib function; `jit_surface::works_from_json_float`
// disproves that and is red for exactly this reason. The fix shape is the
// one the error text names — a dedicated split for `decodeFloat_Int#`
// alongside the existing ones — not a change here.
//
// The fix that landed (a named, loud `error` at extract time instead of
// silently aliasing both result binders to one primop node) was verified
// directly against the extract binary with hand-written
// `MagicHash`/`UnboxedTuples` source hitting the two now-error'd arms:
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
//
// CRASH(stack-death)-class — STANDALONE: this pins a call-depth mechanism
// (the JIT's non-tail-call stack ceiling), not a stdlib function's ordinary
// JIT-safety — mirrors `jit_surface.rs`'s own DISTINCT-MECHANISM exclusion
// class (TCO/call-depth). On regression the probe blows the native stack
// instead of returning a wrong value, which would take out any check
// bundled alongside it.
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
