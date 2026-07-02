//! Rust-side result truncation + the `:stub <n>` fetch affordance.
//!
//! The eval preamble's `paginateResult` used to truncate oversized results
//! HASKELL-side (`paginateTrunc` in `Tidepool.Orchestrate`), discarding the
//! elided subtrees before the value ever crossed into Rust — so the
//! `[~N chars -> stub_K]` markers named stubs nothing could fetch, and the
//! `[truncated — bind the result and re-query]` hint only helped when the
//! value happened to be bindable. The repl instead:
//!
//! 1. patches `paginateResult` to a pass-through ([`passthrough_paginate`],
//!    applied in `server::repl_preamble`) so the FULL value reaches Rust,
//! 2. truncates here with the same marker vocabulary ([`truncate_result`]),
//! 3. stashes the elided subtrees on the session (`Session::last_stubs`,
//!    replaced each time a new truncating result lands), where the
//!    `:stub <n>` meta command ([`stub_fetch`]) retrieves them in full.
//!
//! The marker vocabulary is kept byte-compatible with the Haskell one so
//! existing callers' parsers keep working:
//! - oversized array element / object field → `"[~N chars -> stub_K]"`
//! - budget-exhausted array tail → `"[N more, ~M chars -> stub_K]"`
//! - budget-exhausted object tail → a `"..."` key with
//!   `"[N more fields, ~M chars -> stub_K]"`
//! - oversized top-level string → `"<prefix>...[N chars -> stub_K]"` (the
//!   Haskell version dropped the stub id here — the full string was
//!   unfetchable; stashing it is part of this fix).

use serde_json::{Map, Value};

/// The result-size budget (chars of rendered JSON, approximated by
/// [`val_size`]) — matches the `paginateResult 4096` call the eval template
/// emits, which the pass-through patch turns into a no-op.
pub const RESULT_BUDGET: usize = 4096;

/// Rendered-JSON chars per `:stub` page. Well above the common oversized-field
/// size so a fetch like the 4470-char dogfooding case round-trips in ONE page;
/// genuinely huge stubs (a whole-file readFile) come back paged.
pub const STUB_PAGE_CHARS: usize = 30_000;

/// The `paginateResult` binding line `paginate_alias()` emits for the repl's
/// non-interactive preamble (the type-sig line above it is left intact).
const ALIAS_LINE: &str = "paginateResult = paginateTrunc\n";
const PASSTHROUGH_LINE: &str = "paginateResult _ v = pure v\n";

/// Patch the repl preamble's `paginateResult` alias to a pass-through, so
/// oversized results reach Rust untruncated and [`truncate_result`] (which can
/// stash stubs) runs instead of the Haskell `paginateTrunc` (which cannot).
///
/// String surgery on the generated preamble, same technique as
/// `session::hide_module_names`: replaces the `paginateResult = paginateTrunc`
/// binding line with `paginateResult _ v = pure v`. No-op if the alias line is
/// absent (defensive: an empty effect stack emits no alias).
pub fn passthrough_paginate(preamble: &str) -> String {
    preamble.replacen(ALIAS_LINE, PASSTHROUGH_LINE, 1)
}

/// Approximate rendered-JSON size in chars — port of the Haskell `valSize`
/// (`Tidepool.Orchestrate`): strings count chars + quotes, numbers a flat 8,
/// containers add 2 delimiters + 2 per separator + 4 per object key's `": "`
/// dressing.
fn val_size(v: &Value) -> usize {
    match v {
        Value::String(t) => t.chars().count() + 2,
        Value::Number(_) => 8,
        Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        Value::Null => 4,
        Value::Array(xs) => arr_sz(xs) + 2,
        Value::Object(m) => {
            m.iter().map(|(k, v)| pair_size(k, v)).sum::<usize>()
                + 2 * m.len().saturating_sub(1)
                + 2
        }
    }
}

/// `arrSz xs 0`: element sizes + 2 per separator (0 for an empty slice).
fn arr_sz(xs: &[Value]) -> usize {
    xs.iter().map(val_size).sum::<usize>() + 2 * xs.len().saturating_sub(1)
}

/// `objSz` per-pair term: key chars + 4 (quotes + `: `) + value size.
fn pair_size(k: &str, v: &Value) -> usize {
    k.chars().count() + 4 + val_size(v)
}

/// `objSz kvs 0`: pair sizes + 2 per separator (0 for an empty slice).
fn obj_sz(kvs: &[(String, Value)]) -> usize {
    kvs.iter().map(|(k, v)| pair_size(k, v)).sum::<usize>() + 2 * kvs.len().saturating_sub(1)
}

/// Truncate a rendered result value to [`RESULT_BUDGET`], returning
/// `(truncated, stubs, hint)`:
/// - `truncated` — the value with oversized subtrees replaced by the marker
///   strings listed in the module doc (stub ids `stub_0..` in encounter order),
/// - `stubs` — the elided subtrees, indexed by stub id (empty ⇒ no truncation),
/// - `hint` — `None` when nothing was truncated; otherwise the self-teaching
///   affordance text, EXACTLY naming the fetch command, e.g.
///   `"result truncated — 2 stub(s) elided; fetch the full content with
///   :stub <n> in this session (e.g. :stub 0)"`.
///
/// Port of the Haskell `truncGo`/`truncArr`/`truncKvs` budgeting (see
/// `tidepool-mcp/src/preamble.rs`) with one deliberate improvement: an
/// oversized top-level string is ALSO stashed as a stub (see module doc).
pub fn truncate_result(v: Value) -> (Value, Vec<Value>, Option<String>) {
    if val_size(&v) <= RESULT_BUDGET {
        return (v, Vec::new(), None);
    }
    let bud = RESULT_BUDGET as i64;
    let mut stubs = Vec::new();
    let truncated = match v {
        Value::Array(xs) => Value::Array(trunc_arr(bud, &mut stubs, xs)),
        Value::Object(m) => Value::Object(trunc_kvs(bud, &mut stubs, m)),
        Value::String(t) => {
            // Haskell: keep = max 10 (bud - 30), marker "...[N chars]".
            // Improvement: the marker names a stub id and the FULL string is
            // stashed (Haskell dropped it unfetchably).
            let keep = std::cmp::max(10, RESULT_BUDGET.saturating_sub(30));
            let n = t.chars().count();
            let sid = stubs.len();
            let prefix: String = t.chars().take(keep).collect();
            stubs.push(Value::String(t));
            Value::String(format!("{prefix}...[{n} chars -> stub_{sid}]"))
        }
        other => other,
    };
    let hint = if stubs.is_empty() {
        None
    } else {
        Some(format!(
            "result truncated — {} stub(s) elided; fetch the full content with \
             :stub <n> in this session (e.g. :stub 0)",
            stubs.len()
        ))
    };
    (truncated, stubs, hint)
}

/// Port of `truncArr`: keep elements that fit the running budget (charging
/// size + 2 separator), replace an oversized element with a per-element marker
/// (stashing it, charging a flat 50), and once the budget drops to ≤ 30 stash
/// the whole remaining tail behind one `[N more, …]` marker.
fn trunc_arr(mut bud: i64, stubs: &mut Vec<Value>, mut xs: Vec<Value>) -> Vec<Value> {
    let mut out = Vec::new();
    let mut idx = 0;
    while idx < xs.len() {
        if bud <= 30 {
            let rest = xs.split_off(idx);
            let n = rest.len();
            let tsz = val_size(&rest[0]) + arr_sz(&rest[1..]);
            let sid = stubs.len();
            out.push(Value::String(format!(
                "[{n} more, ~{tsz} chars -> stub_{sid}]"
            )));
            stubs.push(Value::Array(rest));
            return out;
        }
        let sz = val_size(&xs[idx]) as i64;
        if sz <= bud {
            bud -= sz + 2;
            out.push(std::mem::take(&mut xs[idx]));
        } else {
            let sid = stubs.len();
            out.push(Value::String(format!("[~{sz} chars -> stub_{sid}]")));
            stubs.push(std::mem::take(&mut xs[idx]));
            bud -= 50;
        }
        idx += 1;
    }
    out
}

/// Port of `truncKvs`: same budgeting as [`trunc_arr`] with the key's
/// `"key": ` dressing charged per pair; an oversized field's marker sizes the
/// VALUE only (matching the Haskell), and the budget-exhausted tail becomes a
/// `"..."` key whose stub holds the remaining pairs as one object.
fn trunc_kvs(mut bud: i64, stubs: &mut Vec<Value>, m: Map<String, Value>) -> Map<String, Value> {
    let mut pairs: Vec<(String, Value)> = m.into_iter().collect();
    let mut out = Map::new();
    let mut idx = 0;
    while idx < pairs.len() {
        if bud <= 30 {
            let rest = pairs.split_off(idx);
            let n = rest.len();
            let tsz = pair_size(&rest[0].0, &rest[0].1) + obj_sz(&rest[1..]);
            let sid = stubs.len();
            out.insert(
                "...".to_string(),
                Value::String(format!("[{n} more fields, ~{tsz} chars -> stub_{sid}]")),
            );
            stubs.push(Value::Object(rest.into_iter().collect()));
            return out;
        }
        let sz = pair_size(&pairs[idx].0, &pairs[idx].1) as i64;
        let (k, v) = {
            let (k, v) = &mut pairs[idx];
            (std::mem::take(k), std::mem::take(v))
        };
        if sz <= bud {
            bud -= sz + 2;
            out.insert(k, v);
        } else {
            let vsz = val_size(&v);
            let sid = stubs.len();
            out.insert(k, Value::String(format!("[~{vsz} chars -> stub_{sid}]")));
            stubs.push(v);
            bud -= 50;
        }
        idx += 1;
    }
    out
}

/// Fetch a stashed stub for the `:stub <n> [page]` meta command.
///
/// - Unknown `n` → `{"error": "..."}` naming how many stubs are stashed and
///   that stubs are replaced by the next truncating result.
/// - Known `n`, rendered JSON ≤ [`STUB_PAGE_CHARS`] → `{"stub": n, "value":
///   <the full subtree>}` (the round-trip case; `value` is the real JSON, not
///   a string).
/// - Known `n`, larger → `{"stub": n, "page": p, "pages": k, "chunk":
///   "<rendered-JSON slice>", "hint": "fetch the next page with :stub n p+1"}`
///   (out-of-range page → `{"error": ...}` naming `pages`).
pub fn stub_fetch(stubs: &[Value], n: usize, page: Option<usize>) -> Value {
    let Some(v) = stubs.get(n) else {
        return serde_json::json!({
            "error": format!(
                "no stub_{n} stashed ({} stub(s) available; stubs are replaced by the \
                 next truncated result — re-run the producing expression if it's gone)",
                stubs.len()
            ),
        });
    };
    let rendered = v.to_string();
    let total_chars = rendered.chars().count();
    let pages = std::cmp::max(1, total_chars.div_ceil(STUB_PAGE_CHARS));
    let p = page.unwrap_or(0);
    if p >= pages {
        return serde_json::json!({
            "error": format!(
                "stub_{n}: page {p} out of range — {pages} page(s) (0-based; \
                 fetch with :stub {n} <0..{}>)",
                pages - 1
            ),
        });
    }
    if pages == 1 {
        return serde_json::json!({ "stub": n, "value": v });
    }
    let chunk: String = rendered
        .chars()
        .skip(p * STUB_PAGE_CHARS)
        .take(STUB_PAGE_CHARS)
        .collect();
    let hint = if p + 1 < pages {
        format!("fetch the next page with :stub {n} {}", p + 1)
    } else {
        format!("final page ({pages} total); chunks concatenate to the rendered JSON")
    };
    serde_json::json!({
        "stub": n,
        "page": p,
        "pages": pages,
        "chunk": chunk,
        "hint": hint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- passthrough_paginate ----

    #[test]
    fn passthrough_patches_the_real_repl_preamble() {
        let stack = tidepool_handlers::build_minimal_stack();
        let (decls, _ask_tag) = tidepool_handlers::base_decls_with_ask(&stack);
        let pre = tidepool_mcp::build_preamble_non_interactive(&decls, false);
        assert!(
            pre.contains(ALIAS_LINE),
            "expected the paginateTrunc alias binding in the generated preamble:\n{pre}"
        );
        let patched = passthrough_paginate(&pre);
        assert!(
            !patched.contains("paginateTrunc"),
            "alias binding should be gone after patching"
        );
        assert!(
            patched.contains("paginateResult :: Int -> Value -> M Value\n"),
            "the type-sig line must survive the patch"
        );
        assert!(
            patched.contains(PASSTHROUGH_LINE),
            "the pass-through binding must be present"
        );
    }

    #[test]
    fn passthrough_is_noop_without_alias() {
        // Empty effect stack emits no paginateResult alias.
        let pre = tidepool_mcp::build_preamble_non_interactive(&[], false);
        assert!(!pre.contains(ALIAS_LINE));
        assert_eq!(passthrough_paginate(&pre), pre);
    }

    // ---- truncate_result ----

    #[test]
    fn small_value_passes_through_untouched() {
        let v = json!({"a": 1, "b": [true, null, "text"]});
        let (out, stubs, hint) = truncate_result(v.clone());
        assert_eq!(out, v);
        assert!(stubs.is_empty());
        assert_eq!(hint, None);
    }

    #[test]
    fn oversized_object_field_becomes_fetchable_stub() {
        let big = "x".repeat(4470);
        let v = json!({"region": big});
        let (out, stubs, hint) = truncate_result(v);
        // valSize of the string = 4470 chars + 2 quotes.
        assert_eq!(out["region"], json!("[~4472 chars -> stub_0]"));
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0], json!("x".repeat(4470)));
        let hint = hint.expect("truncation must produce a hint");
        assert!(
            hint.contains(":stub <n>") && hint.contains(":stub 0"),
            "hint must name the fetch command: {hint}"
        );
        assert_eq!(
            hint,
            "result truncated — 1 stub(s) elided; fetch the full content with \
             :stub <n> in this session (e.g. :stub 0)"
        );
    }

    #[test]
    fn array_tail_marker_and_stubs_cover_all_elements() {
        // 100 × 100-char strings: budget keeps a prefix, then per-element /
        // tail markers take over. Every elided element must live in a stub.
        let elems: Vec<Value> = (0..100)
            .map(|i| json!(format!("{i:03}").repeat(34) + "z"))
            .collect(); // 103 chars each
        let (out, stubs, hint) = truncate_result(Value::Array(elems.clone()));
        assert!(hint.is_some());
        assert!(!stubs.is_empty());
        let out_arr = out.as_array().expect("array in, array out");
        // The last element is the budget-exhausted tail marker.
        let tail = out_arr.last().unwrap().as_str().unwrap();
        assert!(
            tail.starts_with('[') && tail.contains(" more, ~") && tail.contains(" chars -> stub_"),
            "expected tail marker, got: {tail}"
        );
        // Kept elements + elements inside stubs account for all 100.
        let kept = out_arr.iter().filter(|v| elems.contains(v)).count();
        let stubbed: usize = stubs
            .iter()
            .map(|s| match s {
                Value::Array(xs) => xs.len(),
                _ => 1,
            })
            .sum();
        assert_eq!(kept + stubbed, 100, "no element may be silently dropped");
        // Kept prefix is verbatim.
        assert_eq!(out_arr[0], elems[0]);
    }

    #[test]
    fn oversized_array_element_gets_per_element_marker() {
        // One huge element among small ones — elided individually, not as tail.
        let huge = "h".repeat(5000);
        let v = json!(["small", huge, "also small"]);
        let (out, stubs, hint) = truncate_result(v);
        assert!(hint.is_some());
        let out_arr = out.as_array().unwrap();
        assert_eq!(out_arr[0], json!("small"));
        assert_eq!(out_arr[1], json!("[~5002 chars -> stub_0]"));
        assert_eq!(out_arr[2], json!("also small"));
        assert_eq!(stubs[0], json!("h".repeat(5000)));
    }

    #[test]
    fn object_tail_marker_stashes_remaining_fields() {
        // 100 fields × ~103-char values: prefix kept, tail behind a "..." key.
        let mut m = Map::new();
        for i in 0..100 {
            m.insert(format!("key{i:03}"), json!("v".repeat(100)));
        }
        let (out, stubs, hint) = truncate_result(Value::Object(m.clone()));
        assert!(hint.is_some());
        let out_obj = out.as_object().unwrap();
        let tail = out_obj
            .get("...")
            .expect("budget-exhausted object must carry the \"...\" tail key")
            .as_str()
            .unwrap();
        assert!(
            tail.starts_with('[')
                && tail.contains(" more fields, ~")
                && tail.contains(" chars -> stub_"),
            "expected object tail marker, got: {tail}"
        );
        // Every field is either kept verbatim, elided behind a per-field
        // marker (its own stub), or inside the tail stub — none dropped.
        let is_marker = |v: &Value| {
            v.as_str()
                .is_some_and(|s| s.starts_with("[~") && s.contains(" chars -> stub_"))
        };
        let (kept, marked): (Vec<_>, Vec<_>) = out_obj
            .iter()
            .filter(|(k, _)| *k != "...")
            .partition(|(_, v)| !is_marker(v));
        let tail_stub = stubs.last().unwrap().as_object().unwrap();
        assert_eq!(kept.len() + marked.len() + tail_stub.len(), 100);
        assert_eq!(
            marked.len(),
            stubs.len() - 1,
            "each per-field marker owns one stub (the last stub is the tail)"
        );
        // Kept fields are verbatim.
        for (k, v) in kept {
            assert_eq!(Some(v), m.get(k), "kept field {k} must be untouched");
        }
    }

    #[test]
    fn oversized_top_level_string_is_stashed() {
        let s = "s".repeat(5000);
        let (out, stubs, hint) = truncate_result(json!(s));
        let out_s = out.as_str().unwrap();
        // keep = max(10, 4096 - 30) = 4066 prefix chars.
        assert!(out_s.starts_with(&"s".repeat(4066)));
        assert!(
            out_s.ends_with("...[5000 chars -> stub_0]"),
            "improved marker names the stub: {out_s}"
        );
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0], json!("s".repeat(5000)));
        assert!(hint.unwrap().contains(":stub"));
    }

    // ---- stub_fetch ----

    #[test]
    fn stub_fetch_full_value() {
        let stubs = vec![json!({"k": "v", "n": 7})];
        let got = stub_fetch(&stubs, 0, None);
        assert_eq!(got, json!({"stub": 0, "value": {"k": "v", "n": 7}}));
        // Explicit page 0 of a single-page stub is the same round-trip.
        assert_eq!(stub_fetch(&stubs, 0, Some(0)), got);
    }

    #[test]
    fn stub_fetch_pages_reassemble_to_rendered_whole() {
        let big = json!("y".repeat(70_000));
        let rendered = big.to_string(); // 70_002 chars with quotes
        let stubs = vec![big];
        let pages = rendered.chars().count().div_ceil(STUB_PAGE_CHARS);
        assert_eq!(pages, 3);
        let mut reassembled = String::new();
        for p in 0..pages {
            let got = stub_fetch(&stubs, 0, Some(p));
            assert_eq!(got["stub"], json!(0));
            assert_eq!(got["page"], json!(p));
            assert_eq!(got["pages"], json!(3));
            reassembled.push_str(got["chunk"].as_str().unwrap());
            let hint = got["hint"].as_str().unwrap();
            if p + 1 < pages {
                assert_eq!(hint, format!("fetch the next page with :stub 0 {}", p + 1));
            } else {
                assert!(hint.contains("final page"), "last-page hint: {hint}");
            }
        }
        assert_eq!(reassembled, rendered);
    }

    #[test]
    fn stub_fetch_out_of_range_page_names_pages() {
        let stubs = vec![json!("y".repeat(70_000))];
        let got = stub_fetch(&stubs, 0, Some(3));
        let err = got["error"].as_str().expect("out-of-range page errors");
        assert!(
            err.contains("page 3 out of range") && err.contains("3 page(s)"),
            "error must name the page count: {err}"
        );
        // A single-page stub rejects page 1 the same way.
        let small = vec![json!("tiny")];
        let got = stub_fetch(&small, 0, Some(1));
        assert!(got["error"].as_str().unwrap().contains("1 page(s)"));
    }

    #[test]
    fn stub_fetch_unknown_stub_error() {
        let stubs = vec![json!(1)];
        let got = stub_fetch(&stubs, 5, None);
        let err = got["error"].as_str().expect("unknown stub errors");
        assert!(
            err.contains("no stub_5 stashed")
                && err.contains("1 stub(s) available")
                && err.contains("replaced by the next truncated result"),
            "self-explaining unknown-stub error: {err}"
        );
    }
}
