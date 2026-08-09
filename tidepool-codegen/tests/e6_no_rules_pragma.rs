//! E6 (`GhcPipeline.reachableModuleClosure`) computes its reachability walk
//! on DESUGARED (pre-`core2core`) Core. The invariant it actually needs —
//! not "no RULES pragmas exist", that's a proxy — is: **`core2core` cannot
//! ADD a home-module cross-reference that is absent from desugared Core.**
//! `{-# RULES #-}` is the one mechanism that could violate it: a rewrite
//! rule can replace an expression with a call to an entirely different,
//! otherwise-unreferenced function during `core2core`, after the
//! reachability walk has already run and decided what gets optimized.
//!
//! **Scope is home modules ONLY (`haskell/lib`/`haskell/src`), deliberately
//! — not the package set, and that is not a shortcut, it is exactly
//! coextensive with the risk.** `canonicalizeDFlags` (`GhcPipeline.hs`)
//! disables `Opt_FullLaziness`/`Opt_CprAnal` but does NOT disable rewrite
//! rules (grep-confirmed: no `Opt_EnableRewriteRules` reference anywhere in
//! that file) — package-defined RULES (base's fusion rules and friends) DO
//! fire during `core2core` here. But a RULE's RHS is typechecked and
//! scope-resolved at the RULE's OWN DEFINITION SITE: a package-defined RULE
//! can only name identifiers already in scope in that PACKAGE module —
//! other package code, never a home module, because home modules are
//! supplied via `--include`/`importPaths` entirely outside package
//! resolution and do not exist as far as any installed package's own build
//! is concerned.
//!
//! VERIFIED, not merely argued (extract-wave TL, 2026-08-09): `--include`
//! dirs enter `importPaths` at `GhcPipeline.hs:293` — GHC's SOURCE search
//! path, disjoint from the package database. Re-run that check if you doubt
//! the paragraph above; it is the anchor the whole scope justification rests
//! on. An argument in a comment is a claim, an argument plus its
//! verification anchor is a claim a later reader can re-run rather than
//! having to trust that someone once did.
//!
//! So a package RULE can rewrite one package call into
//! another, but can never CONJURE a reference to, say,
//! `Tidepool.Aeson.Value` — it has no way to name it. The same argument
//! covers inlining and specialisation: a package function's body can never
//! contain a home reference to begin with (same scope argument), so
//! inlining or specialising it cannot introduce one either — both only
//! propagate references that were already present in the inlined/specialised
//! function's own body, never invent new ones. And a rule that ELIMINATES a
//! reference doesn't threaten the invariant either: elimination only makes
//! the desugared-stage graph a strict (harmless) superset of what's
//! actually needed — over-inclusion is already argued harmless above.
//!
//! So the only channel through which a RULE could break the invariant is a
//! HOME-defined `{-# RULES #-}` pragma naming another home module — which is
//! exactly what this test scans for. That argument currently rests on an
//! EMPIRICAL absence (no RULES pragma anywhere in `haskell/lib`/`haskell/
//! src` today, grep-confirmed by hand) — not an enforced one. This test
//! enforces it: it fails loudly, naming the reason, the day one is added,
//! instead of leaving E6's reachability tier silently unsound (a class of
//! mistake no standing gate in the battery would catch — see
//! `tidepool-codegen/tests/haskell_suite_differential.rs` and
//! `real_core_corpus.rs`, both static-fixture differentials that never
//! re-invoke the extractor).
//!
//! Deliberately minimal: this is a guard for E6's assumption, not RULES
//! support, and deliberately NOT widened to scan the package set (that
//! would be unbounded, and per the argument above, pointless — package
//! RULES cannot reach home modules regardless of how many there are). If a
//! HOME-defined RULES pragma is ever legitimately needed, E6's reachability
//! walk needs to be revisited (a RULES-aware closure, or dropping the
//! desugar-stage-is-sufficient argument) before this test's failure should
//! be silenced.
use std::path::{Path, PathBuf};

fn haskell_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../haskell")
}

/// Every `.hs` file under `dir`, recursively. No external walk crate: this
/// tree is small and the dependency isn't worth adding for one test.
fn hs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            hs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "hs") {
            out.push(path);
        }
    }
}

#[test]
fn no_home_rules_pragmas_in_extract_relevant_haskell_source() {
    let root = haskell_root();
    let mut offenders = Vec::new();
    for dir_name in ["lib", "src"] {
        let dir = root.join(dir_name);
        let mut files = Vec::new();
        hs_files(&dir, &mut files);
        for file in files {
            let Ok(contents) = std::fs::read_to_string(&file) else {
                continue;
            };
            // A real pragma is always the first token on its line (GHC's
            // lexer requires `{-#` to open a pragma, not appear mid-line).
            // `trim_start` before matching so this doesn't fire on a `--`
            // line comment or Haddock prose that merely MENTIONS RULES
            // pragmas (as this file's own module-level GhcPipeline.hs
            // commentary does, discussing exactly the assumption this test
            // guards).
            let has_real_pragma = contents
                .lines()
                .any(|line| line.trim_start().starts_with("{-# RULES"));
            if has_real_pragma {
                offenders.push(file);
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "E6's reachability walk (GhcPipeline.reachableModuleClosure) computes on \
         desugared, pre-core2core Core and needs: core2core cannot ADD a \
         home-module cross-reference absent from desugared Core. A HOME-defined \
         {{-# RULES #-}} pragma can violate that (a rewrite rule can replace an \
         expression with a call to an otherwise-unreferenced home function during \
         core2core, after the reachability walk already ran) — silently \
         mis-tiering a module the optimized Core actually needs, a class of \
         mistake no standing gate in the battery would catch. (Package-defined \
         RULES are NOT the concern here and this guard deliberately does not scan \
         for them: a package RULE's RHS is scope-resolved at its own \
         package-module definition site, which cannot name a home module — home \
         modules are supplied via --include/importPaths entirely outside package \
         resolution.) Found HOME RULES pragmas in: {offenders:?}. Before adding \
         one, revisit E6's reachability rule (plans/post-restart/extract-wave/\
         spawn-latency/02-e6-tiered-o2.md)."
    );
}
