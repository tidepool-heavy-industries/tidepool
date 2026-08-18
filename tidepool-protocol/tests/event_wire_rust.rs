//! The pin for `gen::wire_rs`'s Event (`RepoEvent`) output — the same proof
//! `worktree_wire_rust.rs` gives Worktree, extended with the cross-effect
//! `foreign_types` reference (`Watch`/`HeadChangeKind`/`HeadChangeReceipt`/
//! `CommitReceipt` all name Worktree's OWN types) that Worktree's lane never
//! exercised.
//!
//! Two independent proofs, same shape as the Worktree pin:
//!
//! 1. [`event_wire_module_matches_pin`] — the WHOLE emitted module, byte-for-byte
//!    against an insta snapshot (`tests/snapshots/`). A coarse regression net:
//!    it catches ANY change to the emitted module, but its own baseline can be
//!    blindly re-accepted (`cargo insta accept`), so it proves nothing by
//!    itself — test 2 is the independent proof.
//! 2. [`event_wire_types_match_the_hand_written_block_field_for_field`] — per
//!    type, field/variant NAMES and ORDER (positional, never a set), Rust field
//!    types, the exact derive line, and `#[core(name = …)]` presence.
//!
//! Every expected value in test 2 is HAND-TRANSCRIBED from the `Ev*` block that
//! was live in `tidepool-bridge-effects/src/lib.rs` before this lane's flip
//! (`EvEventId` through `EvRepositoryEvent`) — not copied from this generator's
//! own output, for the same reason `worktree_wire_rust.rs`'s header gives. It
//! stays a literal `assert_eq!`, not a snapshot: snapshotting it would seed the
//! baseline from the schema's own output on first run, which is exactly the
//! tautology this test exists to avoid.

use tidepool_protocol::effects;
use tidepool_protocol::gen::wire_rs;

#[test]
fn event_wire_module_matches_pin() {
    let event = effects::event::event();
    let generated = wire_rs::file(&event);
    insta::assert_snapshot!(generated.contents);
}

// ---------------------------------------------------------------------------
// The independent, hand-transcribed structural cross-check.
// ---------------------------------------------------------------------------

const WIRE: &str = "#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]";
const WIRE_COPY: &str = "#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]";
const WIRE_ID_ORD: &str =
    "#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]";
const WIRE_ID_ORD_HASH: &str =
    "#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]";
const WIRE_RET_ONLY: &str = "#[derive(ToCore, Clone, Debug, PartialEq)]";

/// `(rust_field_name, rust_field_type)` pairs inside `pub struct NAME { … }`.
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

/// Variant texts inside `pub enum NAME { … }`, in source order.
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

/// Every field/variant, in order, transcribed by hand from the `Ev*` block
/// that was live in `tidepool-bridge-effects/src/lib.rs` before this lane's
/// flip. See this file's header doc for why that transcription — not the
/// generator's own output — is what this test proves against.
#[test]
fn event_wire_types_match_the_hand_written_block_field_for_field() {
    let event = effects::event::event();
    let module = wire_rs::file(&event).contents;

    assert_struct(
        &module,
        "EvEventId",
        "EventId",
        WIRE_ID_ORD,
        true,
        &[("raw", "i64")],
    );
    assert_struct(
        &module,
        "EvSubscriptionId",
        "SubscriptionId",
        WIRE_ID_ORD_HASH,
        true,
        &[("raw", "i64")],
    );

    assert_enum(
        &module,
        "EvWatch",
        WIRE,
        &[
            "WatchCommit(WtWorktreeId)",
            "WatchHead(WtWorktreeId)",
            "WatchDeadline(i64)",
            "WatchAsync(i64)",
            "WatchMailbox(i64)",
        ],
    );

    assert_enum(
        &module,
        "EvHeadChangeKind",
        WIRE,
        &[
            "Advanced(Vec<WtGitOid>)",
            "Amended(WtGitOid, WtGitOid)",
            "Rewritten(Vec<(WtGitOid, WtGitOid)>)",
            "Rewound",
            "Switched",
            "UnknownChange",
        ],
    );

    assert_struct(
        &module,
        "EvHeadChangeReceipt",
        "HeadChangeReceipt",
        WIRE,
        true,
        &[
            ("head_worktree", "WtWorktreeId"),
            ("old_head", "Option<WtGitOid>"),
            ("new_head", "WtGitOid"),
            ("kind", "EvHeadChangeKind"),
            ("head_branch", "Option<WtBranchName>"),
            ("observed_at_ms", "i64"),
        ],
    );

    assert_struct(
        &module,
        "EvCommitReceipt",
        "CommitReceipt",
        WIRE,
        true,
        &[
            ("commit_worktree", "WtWorktreeId"),
            ("oid", "WtGitOid"),
            ("parents", "Vec<WtGitOid>"),
            ("subject", "String"),
            ("author", "String"),
            ("committed_at_ms", "i64"),
            ("files", "Vec<String>"),
        ],
    );

    assert_struct(
        &module,
        "EvTickReceipt",
        "Tick",
        WIRE_COPY,
        true,
        &[("fired_at_ms", "i64")],
    );

    assert_enum(
        &module,
        "EvRepositoryEvent",
        WIRE_RET_ONLY,
        &[
            "ObservedCommit(EvEventId, EvCommitReceipt)",
            "ObservedHeadChange(EvEventId, EvHeadChangeReceipt)",
            "ObservedTick(EvEventId, EvTickReceipt)",
            "ObservedAsyncDone(EvEventId, i64)",
            "ObservedMessage(EvEventId, i64, serde_json::Value)",
        ],
    );
}

#[test]
fn event_has_wire_types_but_no_adapters() {
    let event = effects::event::event();
    assert!(wire_rs::has_wire_types(&event));
    assert!(!tidepool_protocol::gen::adapter_rs::has_adapters(&event));
}
