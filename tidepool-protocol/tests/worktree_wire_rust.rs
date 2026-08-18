//! The pin for `gen::wire_rs`'s Worktree output — the wire-record analogue of
//! `generated_files_are_current.rs`'s third layer, and the scaffold doc
//! §11.7's Class E ("wire-struct identity").
//!
//! Worktree is deliberately not in [`tidepool_protocol::effects::all`], so
//! nothing here is written to disk — this test calls the generator directly
//! against [`tidepool_protocol::effects::worktree::worktree`].
//!
//! Two independent proofs:
//!
//! 1. [`worktree_wire_module_matches_pin`] — the WHOLE emitted module,
//!    asserted byte-for-byte against an insta snapshot (`tests/snapshots/`).
//!    A coarse regression net: it catches ANY change to the emitted module,
//!    but its own baseline can be blindly re-accepted (`cargo insta accept`),
//!    so it proves nothing by itself — test 2 is the independent proof.
//! 2. [`worktree_wire_types_match_the_hand_written_block_field_for_field`] —
//!    per type, the field/variant NAMES and ORDER (asserted POSITIONALLY, as
//!    a `Vec`, never as a set — a permutation is exactly the failure the
//!    hand-written struct's positional comment stood guard against and a set
//!    comparison would pass straight through), the Rust field types, the
//!    exact derive line, and the presence/absence of `#[core(name = …)]`.
//!
//! Every expected value in test 2 is HAND-TRANSCRIBED by reading the
//! still-live struct block in `tidepool-bridge-effects/src/lib.rs` (roughly
//! lines 105-232, `WtWorktreeId` through `WtWorktreeSummary`) — not copied out
//! of this generator's own output. That transcription is the only thing
//! making the test a proof rather than a tautology: a generator bug that
//! renders the wrong field order, the wrong Rust type, or the wrong derive set
//! would still produce SOME output, and only an independently-authored
//! expectation catches it being the wrong one. It stays a literal
//! `assert_eq!`, not a snapshot: snapshotting it would seed the baseline from
//! the schema's own output on first run, which is exactly the tautology this
//! test exists to avoid.

use tidepool_protocol::effects;
use tidepool_protocol::gen::wire_rs;

#[test]
fn worktree_wire_module_matches_pin() {
    let worktree = effects::worktree::worktree();
    let generated = wire_rs::file(&worktree);
    insta::assert_snapshot!(generated.contents);
}

// ---------------------------------------------------------------------------
// The independent, hand-transcribed structural cross-check.
// ---------------------------------------------------------------------------

const WIRE: &str = "#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]";
const WIRE_COPY: &str = "#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]";
const WIRE_DEFAULT: &str = "#[derive(ToCore, FromCore, Clone, Debug, Default, PartialEq, Eq)]";

/// `(rust_field_name, rust_field_type)` pairs inside `pub struct NAME { … }`,
/// in source order. Skips a field's own doc-comment lines, so a field
/// carrying one (`WtGitFailureReceipt::git_exit_code`) does not shift the
/// parse.
fn struct_fields<'a>(module: &'a str, name: &str) -> Vec<(&'a str, &'a str)> {
    let marker = format!("pub struct {name} {{");
    let start = module
        .find(&marker)
        .unwrap_or_else(|| panic!("no `pub struct {name}` in the generated module"));
    let body = &module[start + marker.len()..];
    let end = body.find("\n}\n").expect("unterminated struct");
    body[..end]
        .lines()
        .filter(|l| l.trim_start().starts_with("pub "))
        .map(|l| {
            let l = l.trim();
            let l = l.strip_prefix("pub ").expect("filtered for `pub ` prefix");
            let l = l.trim_end_matches(',');
            let (field, ty) = l.split_once(':').expect("field line must be `name: Type`");
            (field.trim(), ty.trim())
        })
        .collect()
}

/// Variant texts inside `pub enum NAME { … }`, in source order — e.g.
/// `"SourceRef(WtGitRef)"`, `"RequireClean"`.
fn enum_variants<'a>(module: &'a str, name: &str) -> Vec<&'a str> {
    let marker = format!("pub enum {name} {{");
    let start = module
        .find(&marker)
        .unwrap_or_else(|| panic!("no `pub enum {name}` in the generated module"));
    let body = &module[start + marker.len()..];
    let end = body.find("\n}\n").expect("unterminated enum");
    body[..end]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("///"))
        .map(|l| l.trim_end_matches(','))
        .collect()
}

/// The doc/derive/`#[core(name = …)]` text immediately preceding a
/// `pub struct NAME {` / `pub enum NAME {` declaration — bounded by the
/// nearest preceding blank line, which is how every type is separated in the
/// emitted module.
fn type_prelude<'a>(module: &'a str, marker: &str) -> &'a str {
    let start = module
        .find(marker)
        .unwrap_or_else(|| panic!("no `{marker}` in the generated module"));
    let prelude_start = module[..start].rfind("\n\n").map_or(0, |i| i + 2);
    &module[prelude_start..start]
}

#[allow(clippy::too_many_arguments)]
fn assert_struct(
    module: &str,
    wire_name: &str,
    hs_name: &str,
    derive_line: &str,
    needs_core_name: bool,
    fields: &[(&str, &str)],
) {
    let marker = format!("pub struct {wire_name} {{");
    let prelude = type_prelude(module, &marker);
    assert!(
        prelude.contains(derive_line),
        "{wire_name}: expected derive line `{derive_line}` in:\n{prelude}"
    );
    let core_attr = format!("#[core(name = \"{hs_name}\")]");
    assert_eq!(
        prelude.contains(&core_attr),
        needs_core_name,
        "{wire_name}: #[core(name = \"{hs_name}\")] presence mismatch in:\n{prelude}"
    );
    assert_eq!(
        struct_fields(module, wire_name),
        fields,
        "{wire_name}: field name/type/order mismatch"
    );
}

fn assert_enum(module: &str, wire_name: &str, derive_line: &str, variants: &[&str]) {
    let marker = format!("pub enum {wire_name} {{");
    let prelude = type_prelude(module, &marker);
    assert!(
        prelude.contains(derive_line),
        "{wire_name}: expected derive line `{derive_line}` in:\n{prelude}"
    );
    assert!(
        !prelude.contains("#[core(name"),
        "{wire_name}: an enum must never carry #[core(name = …)] — its data \
         constructors ARE its variant names"
    );
    assert_eq!(
        enum_variants(module, wire_name),
        variants,
        "{wire_name}: variant name/order mismatch"
    );
}

/// Every field/variant, in order, transcribed by hand from
/// `tidepool-bridge-effects/src/lib.rs`'s `WtWorktreeId`..`WtWorktreeSummary`
/// block (lines ~105-232 at the time of writing). See this file's header doc
/// for why that transcription — not the generator's own output — is what this
/// test proves against.
#[test]
fn worktree_wire_types_match_the_hand_written_block_field_for_field() {
    let worktree = effects::worktree::worktree();
    let module = wire_rs::file(&worktree).contents;

    assert_struct(
        &module,
        "WtWorktreeId",
        "WorktreeId",
        WIRE,
        true,
        &[("raw", "String")],
    );
    assert_struct(
        &module,
        "WtGitOid",
        "GitOid",
        WIRE,
        true,
        &[("raw", "String")],
    );
    assert_struct(
        &module,
        "WtGitRef",
        "GitRef",
        WIRE,
        true,
        &[("raw", "String")],
    );
    assert_struct(
        &module,
        "WtBranchName",
        "BranchName",
        WIRE,
        true,
        &[("raw", "String")],
    );

    assert_enum(
        &module,
        "WtWorktreeSource",
        WIRE,
        &[
            "SourceCurrentRepository",
            "SourceRef(WtGitRef)",
            "SourceWorktree(WtWorktreeId)",
        ],
    );

    assert_enum(
        &module,
        "WtDirtyPolicy",
        WIRE_COPY,
        &["RequireClean", "AllowDirtySnapshot"],
    );

    assert_struct(
        &module,
        "WtWorktreeSpec",
        "WorktreeSpec",
        WIRE,
        true,
        &[
            ("spec_source", "WtWorktreeSource"),
            ("spec_label", "String"),
            ("spec_dirty_policy", "WtDirtyPolicy"),
        ],
    );

    assert_enum(
        &module,
        "WtInProgressKind",
        WIRE_COPY,
        &[
            "InProgressMerge",
            "InProgressRebase",
            "InProgressCherryPick",
            "InProgressRevert",
            "InProgressBisect",
        ],
    );

    assert_struct(
        &module,
        "WtDirtySummary",
        "DirtySummary",
        WIRE_DEFAULT,
        true,
        &[
            ("staged", "Vec<String>"),
            ("unstaged", "Vec<String>"),
            ("untracked", "Vec<String>"),
            ("ignored_excluded", "i64"),
        ],
    );

    assert_struct(
        &module,
        "WtGitFailureReceipt",
        "GitFailureReceipt",
        WIRE,
        true,
        &[
            ("git_args", "Vec<String>"),
            ("git_cwd", "String"),
            ("git_exit_code", "Option<i64>"),
            ("git_stdout", "String"),
            ("git_stderr", "String"),
        ],
    );

    assert_struct(
        &module,
        "WtWorktreeReceipt",
        "WorktreeReceipt",
        WIRE,
        true,
        &[
            ("tree_id", "WtWorktreeId"),
            ("cwd", "String"),
            ("branch", "WtBranchName"),
            ("source_head", "WtGitOid"),
            ("snapshot_ref", "Option<WtGitRef>"),
            ("created_at", "i64"),
        ],
    );

    assert_struct(
        &module,
        "WtWorktreeHandle",
        "WorktreeHandle",
        WIRE,
        true,
        &[("handle_receipt", "WtWorktreeReceipt")],
    );

    assert_struct(
        &module,
        "WtWorktreeSummary",
        "WorktreeSummary",
        WIRE,
        true,
        &[
            ("summary_receipt", "WtWorktreeReceipt"),
            ("present", "bool"),
        ],
    );
}

// ---------------------------------------------------------------------------
// has_wire_types / module_index
// ---------------------------------------------------------------------------

#[test]
fn exec_and_journal_have_no_wire_types() {
    assert!(!wire_rs::has_wire_types(&effects::exec::exec()));
    assert!(!wire_rs::has_wire_types(&effects::journal::journal()));
}

#[test]
fn module_index_lists_only_effects_with_wire_types() {
    let described = effects::all_described();
    let index = wire_rs::module_index(&described).contents;

    assert!(index.contains("pub mod worktree;"));
    assert!(index.contains("pub use worktree::*;"));
    assert!(!index.contains("pub mod exec;"));
    assert!(!index.contains("pub mod journal;"));
}
