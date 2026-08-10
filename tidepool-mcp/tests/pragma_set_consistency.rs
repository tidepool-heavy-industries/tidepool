//! generic-surface wave, item 4 — the canonical extension-set reconciliation
//! the external review asked for after the pragma-vs-flags redesign: "ONE
//! canonical extension-set definition with tested per-surface DELTAS... plus
//! a set-equality test between the Harness.Prelude re-export module and the
//! profile."
//!
//! Four extension-set surfaces exist across the workspace and legitimately
//! differ in scope — that's fine, as long as every delta is DECLARED and
//! TESTED rather than incidental:
//!
//!   - `tidepool_mcp::preamble::EVAL_PRAGMAS` — the canonical eval dialect.
//!   - `tidepool_mcp::preamble::decl_pragmas()` — EVAL_PRAGMAS +
//!     `NoMonomorphismRestriction` (session decl modules generalize pure binds).
//!   - the `LANGUAGE` block inside
//!     `tidepool_runtime::session::turn::DECL_TEMPLATE_SOURCE` — a PARSE-ONLY
//!     subset (no typecheck/rename, so type-inference-affecting extensions are
//!     correctly absent).
//!   - `tidepool_runtime::session::render::ModuleEnv::standalone_default()`'s
//!     `pragmas` — must track `decl_pragmas()` exactly except for
//!     `NoImplicitPrelude` (the standalone lens-free surface relies on the
//!     implicit Prelude import instead of `Tidepool.Prelude`).
//!   - The Haskell-side harness compilation profile
//!     (`haskell/app/Main.hs`'s `harnessProfilePragmaLine`, generic-surface
//!     wave item 4 PART 2) — a Haskell string literal can't import a Rust
//!     constant across the language boundary, so it's checked here by
//!     reading the source file and parsing its extension set directly. It is
//!     designed to carry EXACTLY EVAL_PRAGMAS's set (an author using
//!     `Tidepool.Harness.Prelude` gets the identical dialect an eval author
//!     gets — "one dialect everywhere", repo CLAUDE.md).
//!
//! `tidepool-runtime` sits BELOW `tidepool-mcp` in the workspace dependency
//! graph and cannot import `EVAL_PRAGMAS` itself (documented in both
//! `binders.rs` and `render.rs`) — this crate is where a cross-surface
//! comparison becomes possible, the same reason `tidepool-testing`'s
//! `mock_stack_matches_production` (a lower-layer hand-maintained mirror
//! checked from a higher layer that can see both sides) already lives where
//! it does.

use std::collections::BTreeSet;
use std::path::Path;

use tidepool_mcp::{decl_pragmas, EVAL_PRAGMAS};
use tidepool_runtime::session::render::ModuleEnv;
use tidepool_runtime::session::turn::DECL_TEMPLATE_SOURCE;

/// Parse a `{-# LANGUAGE A, B, C #-}` block (or a bare `A, B, C` extension
/// list, no pragma delimiters) into its set of extension names. Panics on a
/// block with no extension names — every real surface has at least one, and
/// a silently-empty parse would make every test below vacuously pass.
fn extension_set(pragma_text: &str) -> BTreeSet<&str> {
    let inner = pragma_text
        .trim()
        .trim_start_matches("{-#")
        .trim_start()
        .trim_start_matches("LANGUAGE")
        .trim_end()
        .trim_end_matches("#-}")
        .trim();
    let set: BTreeSet<&str> = inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        !set.is_empty(),
        "parsed an empty extension set from: {pragma_text:?}"
    );
    set
}

/// Slice the leading `{-# LANGUAGE ... #-}` block out of a full module
/// template (the rest is the module header and the `{{TURN}}` splice point).
fn pragma_block_of(template: &str) -> &str {
    let end = template
        .find("#-}")
        .expect("decl template must open with a LANGUAGE pragma block");
    &template[..end + "#-}".len()]
}

/// The decl template's parse-only pragma block must be an exact SUBSET of
/// `EVAL_PRAGMAS` (canonical eval dialect) — no extension present there that
/// eval doesn't also carry. This is the "declared delta, not incidental"
/// check: the excluded set is asserted exactly, so an unexplained shrink or
/// grow on EITHER side fails loud instead of silently drifting.
#[test]
fn binder_parse_pragmas_is_exact_subset_of_eval_pragmas() {
    let eval = extension_set(EVAL_PRAGMAS);
    let binder = extension_set(pragma_block_of(DECL_TEMPLATE_SOURCE));

    let extra: Vec<_> = binder.difference(&eval).collect();
    assert!(
        extra.is_empty(),
        "the decl template carries extensions EVAL_PRAGMAS doesn't: {extra:?} — \
         a parse-only surface must never exceed the eval dialect"
    );

    // The documented delta: type-inference/instance-resolution/scoping
    // extensions a PARSE-ONLY pass (no typecheck, no rename) never needs.
    let expected_missing: BTreeSet<&str> = [
        "NoImplicitPrelude",
        "FlexibleContexts",
        "FlexibleInstances",
        "UndecidableInstances",
        "PartialTypeSignatures",
        "ExtendedDefaultRules",
        "BlockArguments",
        "NumericUnderscores",
        "MultilineStrings",
        "DeriveGeneric",
        "DeriveAnyClass",
        "DuplicateRecordFields",
        "OverloadedRecordDot",
    ]
    .into_iter()
    .collect();
    let actual_missing: BTreeSet<&str> = eval.difference(&binder).copied().collect();
    assert_eq!(
        actual_missing, expected_missing,
        "the decl template's gap from EVAL_PRAGMAS changed — update the \
         documented delta (in this test AND DECL_TEMPLATE_SOURCE's doc \
         comment) if the change is intentional"
    );
}

/// `ModuleEnv::standalone_default()`'s pragma set must equal `decl_pragmas()`'s
/// set with EXACTLY ONE documented delta: `NoImplicitPrelude` removed (the
/// standalone lens-free surface relies on the implicit Prelude import
/// instead of `Tidepool.Prelude`). This is the exact drift the external
/// review's redesign-ordered follow-up caught live: `DeriveGeneric`/
/// `DeriveAnyClass` had silently fallen out of sync (generic-surface wave,
/// 2026-08-08) before this test existed.
#[test]
fn standalone_default_tracks_decl_pragmas_modulo_no_implicit_prelude() {
    let decl_pragmas_text = decl_pragmas();
    let decl = extension_set(&decl_pragmas_text);
    let standalone_pragmas = ModuleEnv::standalone_default().pragmas;
    let standalone = extension_set(&standalone_pragmas);

    let mut expected = decl.clone();
    expected.remove("NoImplicitPrelude");

    assert_eq!(
        standalone, expected,
        "ModuleEnv::standalone_default()'s pragma set diverged from \
         decl_pragmas() by more than the documented NoImplicitPrelude delta \
         — see render.rs's ModuleEnv::standalone_default haddock"
    );
}

/// The Haskell-side harness compilation profile
/// (`haskell/app/Main.hs`'s `harnessProfilePragmaLine`) must carry EXACTLY
/// `EVAL_PRAGMAS`'s extension set — an author importing
/// `Tidepool.Harness.Prelude` under `--harness-profile` gets the identical
/// dialect an ordinary eval author gets. Reads the Haskell SOURCE FILE and
/// parses the string literal directly (a Haskell string constant can't be
/// imported into a Rust test across the language boundary) — brittle to the
/// exact literal's formatting by design, so a hand-edit that breaks the
/// parse is itself a signal the mirror needs re-checking.
#[test]
fn haskell_harness_profile_pragma_line_matches_eval_pragmas() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main_hs = manifest_dir
        .parent()
        .expect("tidepool-mcp has a parent (repo root)")
        .join("haskell/app/Main.hs");
    let src = std::fs::read_to_string(&main_hs)
        .unwrap_or_else(|e| panic!("read {}: {e}", main_hs.display()));

    const MARKER: &str = "harnessProfilePragmaLine =";
    let after_marker = src
        .split_once(MARKER)
        .unwrap_or_else(|| panic!("{MARKER} not found in {}", main_hs.display()))
        .1;
    let quote_start = after_marker
        .find('"')
        .unwrap_or_else(|| panic!("no opening quote after {MARKER} in {}", main_hs.display()));
    let after_open = &after_marker[quote_start + 1..];
    let quote_end = after_open
        .find('"')
        .unwrap_or_else(|| panic!("no closing quote after {MARKER} in {}", main_hs.display()));
    let pragma_line = &after_open[..quote_end];

    let eval = extension_set(EVAL_PRAGMAS);
    let harness = extension_set(pragma_line);
    assert_eq!(
        harness, eval,
        "harnessProfilePragmaLine's extension set no longer matches \
         EVAL_PRAGMAS — the harness profile and eval must stay the same \
         dialect (update whichever side drifted)"
    );
}
