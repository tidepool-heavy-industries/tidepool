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

/// The result-size budget (chars of rendered JSON, approximated by
/// [`val_size`]) — matches the `paginateResult 4096` call the eval template
/// emits, which the pass-through patch turns into a no-op.
pub const RESULT_BUDGET: usize = 4096;

/// Rendered-JSON chars per `:stub` page. Well above the common oversized-field
/// size so a fetch like the 4470-char dogfooding case round-trips in ONE page;
/// genuinely huge stubs (a whole-file readFile) come back paged.
pub const STUB_PAGE_CHARS: usize = 30_000;

/// Patch the repl preamble's `paginateResult` alias to a pass-through, so
/// oversized results reach Rust untruncated and [`truncate_result`] (which can
/// stash stubs) runs instead of the Haskell `paginateTrunc` (which cannot).
///
/// String surgery on the generated preamble, same technique as
/// `session::hide_module_names`: replaces the `paginateResult = paginateTrunc`
/// binding line with `paginateResult _ v = pure v`. No-op if the alias line is
/// absent (defensive: an empty effect stack emits no alias).
pub fn passthrough_paginate(preamble: &str) -> String {
    // TODO(child repl-stubs): implement per the doc above.
    preamble.to_string()
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
pub fn truncate_result(
    v: serde_json::Value,
) -> (serde_json::Value, Vec<serde_json::Value>, Option<String>) {
    // TODO(child repl-stubs): implement per the doc above.
    (v, Vec::new(), None)
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
pub fn stub_fetch(stubs: &[serde_json::Value], n: usize, page: Option<usize>) -> serde_json::Value {
    match stubs.get(n) {
        None => serde_json::json!({
            "error": format!(
                "no stub_{n} stashed ({} stub(s) available; stubs are replaced by the \
                 next truncated result — re-run the producing expression if it's gone)",
                stubs.len()
            ),
        }),
        // TODO(child repl-stubs): paging per the doc above (this placeholder
        // returns the full value unpaged).
        Some(v) => serde_json::json!({ "stub": n, "page": page.unwrap_or(0), "value": v }),
    }
}
