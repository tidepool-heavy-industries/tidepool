//! Pin tests for `gen::adapter_rs`'s Worktree output.
//!
//! Worktree is deliberately absent from [`tidepool_protocol::effects::all`], so
//! `generated_files_are_current` never sees this module — nothing is written to
//! disk on this branch. These tests exercise `gen::adapter_rs::file` directly
//! against `effects::all_described()`, which is the test-only view that
//! includes Worktree.

use tidepool_protocol::gen::{adapter_rs, handler_rs};

fn worktree_adapter_text() -> String {
    let worktree = tidepool_protocol::effects::worktree::worktree();
    adapter_rs::file(&worktree).contents
}

/// Whole-module byte pin — the first and strictest layer, now an insta
/// snapshot (`tests/snapshots/`) rather than a hardcoded literal. A coarse
/// regression net: it catches ANY change to the emitted module, but its own
/// baseline can be blindly re-accepted (`cargo insta accept`) — the tests
/// below it are the independent proof that do not have that weakness.
#[test]
fn worktree_adapter_module_text_is_pinned() {
    insta::assert_snapshot!(worktree_adapter_text());
}

/// Independent of the whole-text pin above: every `pub(crate) fn` in the
/// module, in the ORDER it appears. Four `IdentityRaw` into_wire, two
/// `IdentityRaw` from_wire (`GitRef`, `BranchName` — the latter added once
/// `WorktreeMergeInto` needed a `BranchName` argument), one `VariantMap`
/// from_wire (`DirtyPolicy`), one `VariantMap` into_wire (`InProgressKind`) —
/// eight total.
#[test]
fn worktree_adapter_functions_appear_in_order() {
    let text = worktree_adapter_text();
    let names: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("pub(crate) fn "))
        .map(|rest| rest.split(['(', ' ']).next().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "worktree_id_to_wire",
            "git_oid_to_wire",
            "git_ref_to_wire",
            "git_ref_from_wire",
            "branch_name_to_wire",
            "branch_name_from_wire",
            "dirty_policy_from_wire",
            "in_progress_kind_to_wire",
        ]
    );
}

/// The `InProgressKind` variant map, positionally — a permutation of the same
/// five pairs would pass a set comparison but silently swap which domain
/// variant reports as which wire variant. Cross-checked BY HAND against
/// `in_progress_kind_to_wire` in `tidepool-handlers/src/handlers/worktree.rs`
/// (lines 121-129 as of the commit this test was written against): `Merge` ->
/// `InProgressMerge`, `Rebase` -> `InProgressRebase`, `CherryPick` ->
/// `InProgressCherryPick`, `Revert` -> `InProgressRevert`, `Bisect` ->
/// `InProgressBisect`, in exactly that order.
#[test]
fn in_progress_kind_variant_map_is_positional() {
    let text = worktree_adapter_text();
    let expected_pairs: Vec<(&str, &str)> = vec![
        ("Merge", "InProgressMerge"),
        ("Rebase", "InProgressRebase"),
        ("CherryPick", "InProgressCherryPick"),
        ("Revert", "InProgressRevert"),
        ("Bisect", "InProgressBisect"),
    ];
    let mut cursor = 0;
    for (domain_variant, wire_variant) in expected_pairs {
        let needle =
            format!("InProgressKind::{domain_variant} => WtInProgressKind::{wire_variant},");
        let found = text[cursor..]
            .find(&needle)
            .unwrap_or_else(|| panic!("arm `{needle}` missing, or out of position"));
        cursor += found + needle.len();
    }
}

/// Every `HandWritten` conversion generates NO function — only the comment
/// recording why. Checked both ways: no function DEFINITION exists for it
/// (some reasons legitimately mention a name like `worktree_id_from_wire` in
/// backtick-quoted prose, so the check anchors on `fn <name>(`, not a bare
/// substring), and (where the schema records one) the reason text is present
/// verbatim.
#[test]
fn hand_written_conversions_are_commented_not_generated() {
    let text = worktree_adapter_text();

    for missing_fn in [
        "receipt_to_wire",
        "spec_from_wire",
        "worktree_source_from_wire",
        "git_failure_receipt_to_wire",
        "dirty_summary_to_wire",
        "worktree_id_from_wire",
        "error_to_wire",
        "merge_outcome_to_wire",
    ] {
        assert!(
            !text.contains(&format!("fn {missing_fn}(")),
            "`{missing_fn}` must not be generated — its conversion is HandWritten"
        );
    }

    for reason in [
        "the rejection must become a DOMAIN error (`WorktreeNotRegistered`); only the path-safety check is generated, as this type's boundary constructor",
        "composes a FALLIBLE conversion (`worktree_id_from_wire`); the error path is semantic",
        "composes a FALLIBLE conversion; the error path is semantic",
        "`usize` → `i64` widening on `ignoredExcluded`, and the three lists are cloned out of a borrow",
        "`PathBuf` → lossy `String`, `Option<i32>` → `Option<i64>`",
        "field renames (`worktree_id`→`tree_id`, `created_at_ms`→`created_at`) plus a `PathBuf` → lossy `String`",
        "`Merged` wraps its `GitOid` through `git_oid_to_wire`; `Conflict` clones its path `Vec` — both need a conversion beyond a bare variant rename",
    ] {
        assert!(
            text.contains(reason),
            "recorded HandWritten reason missing from the generated module: {reason}"
        );
    }
}

/// Exec and Journal both have empty `type_defs` (no `DomainMap` anywhere), so
/// neither gets an adapter module — same rule `wire_rs::has_wire_types`
/// follows for the wire module.
#[test]
fn exec_and_journal_have_no_adapters() {
    assert!(!adapter_rs::has_adapters(
        &tidepool_protocol::effects::exec::exec()
    ));
    assert!(!adapter_rs::has_adapters(
        &tidepool_protocol::effects::journal::journal()
    ));
}

/// `handler_rs::module_index` lists the adapter module alongside the effect's
/// own generated module for every DESCRIBED effect — including Worktree,
/// which `effects::all()` deliberately omits. This is the index a later flip
/// lane writes into; it must already know Worktree's adapter module exists.
#[test]
fn worktree_adapters_module_is_indexed() {
    let effects = tidepool_protocol::effects::all_described();
    let index = handler_rs::module_index(&effects).contents;
    assert!(
        index.contains("pub mod worktree;"),
        "index is missing the worktree module:\n{index}"
    );
    assert!(
        index.contains("pub mod worktree_adapters;"),
        "index is missing the worktree_adapters module:\n{index}"
    );
}
