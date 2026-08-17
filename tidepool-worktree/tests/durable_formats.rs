//! Class D — durable-format goldens (PRD 22 §11.7 of
//! `plans/self-iterating-harness/22-p1-protocol-scaffold.md`).
//!
//! Captured from the LIVE hand-written types on unmodified trunk, before any
//! line of the phase-3 wire-record generator exists. These goldens prove the
//! Worktree effect migration does not move a single persisted byte on
//! operator machines — same discipline as §5's Class A goldens, applied to
//! disk instead of to the wire.
//!
//! ## Durable roots (grep of every `serde_json::to_*`/`from_*` call site
//! under `tidepool-worktree/src`, transitive closure of every type reachable
//! from each)
//!
//! 1. **[`WorktreeReceipt`]** — `tidepool-worktree/src/registry.rs`. One JSON
//!    file per worktree id at `<registry_root>/records/<id>.json`. Write:
//!    [`WorktreeRegistry::put`](tidepool_worktree::WorktreeRegistry::put) via
//!    `write_atomic` + `serde_json::to_vec_pretty`. Read:
//!    `WorktreeRegistry::get`/`list` via `serde_json::from_slice`.
//!    Closure: [`WorktreeId`], `PathBuf` (×2), [`BranchName`], [`GitOid`],
//!    `Option<`[`GitRef`]`>`, [`WorktreeOrigin`] (`CurrentRepository` |
//!    `Ref(GitRef)` | `Worktree(WorktreeId)`), `i64`,
//!    [`WorktreeRecordStatus`] (`Provisional` | `Finalized`).
//!
//! 2. **`Vec<`[`Binding`]`>`** — `tidepool-worktree/src/binding.rs`. One JSON
//!    file per worktree id at `<binding_root>/<worktree_id>.json`, holding
//!    that worktree's full lease history as a JSON array. Write:
//!    `BindingTable::persist` via `serde_json::to_vec_pretty`. Read:
//!    `BindingTable::open` via `serde_json::from_slice::<Vec<Binding>>`.
//!    Closure: `Binding { `[`WorktreeId`]`, `[`AgentRef`]`, `[`BindingState`]`
//!    (`Active` | `Terminal` | `Released`), i64 }`.
//!
//! 3. **[`JournalEntry`]** — `tidepool-worktree/src/journal.rs`. One JSON
//!    object per line (JSONL), appended to the event-journal file. Write:
//!    `EventJournal::append` via `serde_json::to_string`. Read:
//!    `EventJournal::open` via `serde_json::from_str`, one call per line.
//!    Closure: `u64`, [`EventId`]`(u64)`, [`RepositoryEvent`]
//!    (`HeadChanged(`[`HeadChangeReceipt`]`)` | `Commit(`[`CommitReceipt`]`)`),
//!    `i64`. `HeadChangeReceipt`: [`WorktreeId`], `Option<GitOid>`, `GitOid`,
//!    [`HeadChangeKind`] (`Advanced(Vec<GitOid>)` | `Amended(GitOid, GitOid)`
//!    | `Rewritten(Vec<(GitOid, GitOid)>)` | `Rewound` | `Switched` |
//!    `UnknownChange`), `Option<BranchName>`, `i64`. `CommitReceipt`:
//!    `WorktreeId`, `GitOid`, `Vec<GitOid>`, `String`, `String`, `i64`,
//!    `Vec<String>`.
//!
//! Goldens live under `tests/goldens/durable/`, one file per sample, pinned
//! byte-for-byte via `serde_json::to_string_pretty`. Regenerate (only after a
//! deliberate, reviewed change to one of the types above) with
//! `TIDEPOOL_REGEN_WORKTREE_GOLDENS=1 cargo test -p tidepool-worktree
//! --test durable_formats`.
//!
//! ## Checked and found NOT durable
//!
//! Every OTHER type in this crate carrying `#[derive(Serialize,
//! Deserialize)]`, and why it is excluded — checked by grepping every write
//! and read site above, not assumed:
//!
//! - **[`WorktreeSummary`]** (`registry.rs`) — wraps a `WorktreeReceipt` with
//!   a `present: bool` recomputed fresh on every call. Only ever constructed
//!   in memory by `WorktreeRegistry::list` and handed back to a caller; never
//!   the argument to a `serde_json::to_*`/`from_*` call anywhere in this
//!   crate.
//! - **`Observed<T>`** (`monitor.rs`) — the runtime envelope
//!   `WorktreeMonitor::reconcile` returns to its caller. `JournalEntry` is
//!   what actually gets persisted (its `event`/`event_id` fields are lifted
//!   out of `Observed`, and `cursor`/`recorded_at_ms` are added); `Observed`
//!   itself is never serialized.
//! - **`SubscriptionId`** (`id.rs`) — runtime identity of one live
//!   subscription (`withHandler`'s registration). Process-local by design;
//!   never written by this crate.
//! - **[`WorktreeError`]** and its payload types **[`DirtySummary`]**,
//!   **[`GitFailureReceipt`]**, **[`InProgressKind`]** (`error.rs`) — every
//!   one derives `Serialize`/`Deserialize`, but none is ever the argument to
//!   a `serde_json::to_*`/`from_*` call in `registry.rs`, `binding.rs`, or
//!   `journal.rs`. They cross this crate's boundary only as the `Err` arm of
//!   a `Result`, converted to Haskell via `ToCore`/`FromCore` in a different
//!   crate (`tidepool-bridge-effects`/`tidepool-handlers`) — a distinct
//!   mechanism from durable JSON, and out of this test's scope. §11.7 of the
//!   scaffold doc names "`WorktreeError` and its payload types" among the
//!   durable-format proof obligation; that reference is honored below as a
//!   Layer-2 hardcoded pin (its JSON shape is part of the effect-error
//!   contract other crates already depend on staying stable), not as a
//!   Class-D golden — a golden would misrepresent it as an on-disk durable
//!   format when grep shows it is not one here.

use std::path::{Path, PathBuf};

use tidepool_worktree::testing::binding_row;
use tidepool_worktree::{
    AgentRef, Binding, BindingState, BranchName, CommitReceipt, EventId, GitOid, GitRef,
    HeadChangeKind, HeadChangeReceipt, JournalEntry, RepositoryEvent, WorktreeError, WorktreeId,
    WorktreeOrigin, WorktreeReceipt, WorktreeRecordStatus,
};

// --- golden plumbing, mirroring tidepool-handlers/tests/bridged_records.rs --

fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/durable")
}

const REGEN_VAR: &str = "TIDEPOOL_REGEN_WORKTREE_GOLDENS";

/// Serialize `value` with `serde_json::to_string_pretty`, assert it matches
/// the committed golden `name` byte-for-byte, and return the JSON so the
/// caller can also round-trip it. Regenerate with `TIDEPOOL_REGEN_WORKTREE_GOLDENS=1`.
fn assert_golden<T: serde::Serialize>(name: &str, value: &T) -> String {
    let json = serde_json::to_string_pretty(value)
        .unwrap_or_else(|e| panic!("failed to serialize golden {name}: {e}"));
    let path = goldens_dir().join(name);
    let regen = std::env::var_os(REGEN_VAR).is_some();
    let current = std::fs::read_to_string(&path).ok();
    if regen || current.is_none() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                panic!(
                    "failed to create {} for golden {name}: {e}",
                    parent.display()
                )
            });
        }
        std::fs::write(&path, &json).unwrap_or_else(|e| {
            panic!("failed to write golden {} at {}: {e}", name, path.display())
        });
        if current.as_deref() != Some(json.as_str()) && !regen {
            panic!(
                "golden {} did not exist yet at {} — wrote it, re-run the test \
                 (or set {REGEN_VAR}=1 if this was a deliberate format change)",
                name,
                path.display()
            );
        }
        return json;
    }
    assert_eq!(
        current.unwrap(),
        json,
        "durable-format golden {} at {} drifted from the live type's \
         serde_json::to_string_pretty output — this means the Worktree \
         migration (or some other change) moved a byte of an on-disk \
         durable format. If this is a deliberate, reviewed change, \
         regenerate with {REGEN_VAR}=1 cargo test -p tidepool-worktree \
         --test durable_formats",
        name,
        path.display()
    );
    json
}

fn oid(hex_digit: char) -> GitOid {
    GitOid::from_raw(std::iter::repeat_n(hex_digit, 40).collect::<String>())
}

// --- WorktreeReceipt samples: WorktreeOrigin's 3 variants, WorktreeRecordStatus's
// --- 2 variants, and Option<GitRef>'s Some/None, spread across 3 samples. ----

fn receipt_provisional() -> WorktreeReceipt {
    WorktreeReceipt {
        worktree_id: WorktreeId::from_raw("wt-provisional-0001"),
        cwd: PathBuf::from("/var/tidepool/worktrees/wt-provisional-0001"),
        branch: BranchName::from_raw("tidepool/worktree/sample-wt-provisional-0001"),
        source_head: oid('a'),
        snapshot_ref: None,
        origin: WorktreeOrigin::CurrentRepository,
        source_repository: PathBuf::from("/home/dev/repo"),
        created_at_ms: 1_700_000_000_000,
        status: WorktreeRecordStatus::Provisional,
    }
}

fn receipt_finalized_ref_origin() -> WorktreeReceipt {
    WorktreeReceipt {
        worktree_id: WorktreeId::from_raw("wt-finalized-0002"),
        cwd: PathBuf::from("/var/tidepool/worktrees/wt-finalized-0002"),
        branch: BranchName::from_raw("tidepool/worktree/sample-wt-finalized-0002"),
        source_head: oid('b'),
        snapshot_ref: Some(GitRef::from_raw(
            "refs/tidepool/snapshots/wt-finalized-0002",
        )),
        origin: WorktreeOrigin::Ref(GitRef::from_raw("refs/heads/main")),
        source_repository: PathBuf::from("/home/dev/repo"),
        created_at_ms: 1_700_000_001_000,
        status: WorktreeRecordStatus::Finalized,
    }
}

fn receipt_finalized_worktree_origin() -> WorktreeReceipt {
    WorktreeReceipt {
        worktree_id: WorktreeId::from_raw("wt-finalized-0003"),
        cwd: PathBuf::from("/var/tidepool/worktrees/wt-finalized-0003"),
        branch: BranchName::from_raw("tidepool/worktree/sample-wt-finalized-0003"),
        source_head: oid('c'),
        snapshot_ref: None,
        origin: WorktreeOrigin::Worktree(WorktreeId::from_raw("wt-parent-0000")),
        source_repository: PathBuf::from("/home/dev/repo"),
        created_at_ms: 1_700_000_002_000,
        status: WorktreeRecordStatus::Finalized,
    }
}

// --- Binding sample: all 3 BindingState variants, as one worktree's lease
// --- history (the actual on-disk shape — one file per worktree id). --------

fn binding_rows() -> Vec<Binding> {
    vec![
        binding_row(
            WorktreeId::from_raw("wt-provisional-0001"),
            AgentRef::from_raw("agent-alpha"),
            BindingState::Terminal,
            1_700_000_000_000,
        ),
        binding_row(
            WorktreeId::from_raw("wt-provisional-0001"),
            AgentRef::from_raw("agent-beta"),
            BindingState::Released,
            1_700_000_001_000,
        ),
        binding_row(
            WorktreeId::from_raw("wt-provisional-0001"),
            AgentRef::from_raw("agent-gamma"),
            BindingState::Active,
            1_700_000_002_000,
        ),
    ]
}

// --- Journal sample: all 6 HeadChangeKind variants, Option<GitOid>/
// --- Option<BranchName> Some AND None, and both Commit shapes (root commit
// --- with no parents, merge commit with two). ------------------------------

fn journal_entries() -> Vec<JournalEntry> {
    let wt = WorktreeId::from_raw("wt-finalized-0002");
    vec![
        JournalEntry {
            cursor: 1,
            event_id: EventId(1),
            event: RepositoryEvent::HeadChanged(HeadChangeReceipt {
                worktree: wt.clone(),
                old_head: Some(oid('1')),
                new_head: oid('2'),
                kind: HeadChangeKind::Advanced(vec![oid('3'), oid('4')]),
                branch: Some(BranchName::from_raw(
                    "tidepool/worktree/sample-wt-finalized-0002",
                )),
                observed_at_ms: 1_700_000_010_000,
            }),
            recorded_at_ms: 1_700_000_010_001,
        },
        JournalEntry {
            cursor: 2,
            event_id: EventId(2),
            event: RepositoryEvent::HeadChanged(HeadChangeReceipt {
                worktree: wt.clone(),
                old_head: Some(oid('2')),
                new_head: oid('5'),
                kind: HeadChangeKind::Amended(oid('2'), oid('5')),
                // None on a detached HEAD.
                branch: None,
                observed_at_ms: 1_700_000_020_000,
            }),
            recorded_at_ms: 1_700_000_020_001,
        },
        JournalEntry {
            cursor: 3,
            event_id: EventId(3),
            event: RepositoryEvent::HeadChanged(HeadChangeReceipt {
                worktree: wt.clone(),
                old_head: Some(oid('5')),
                new_head: oid('6'),
                kind: HeadChangeKind::Rewritten(vec![(oid('7'), oid('8')), (oid('9'), oid('0'))]),
                branch: Some(BranchName::from_raw(
                    "tidepool/worktree/sample-wt-finalized-0002",
                )),
                observed_at_ms: 1_700_000_030_000,
            }),
            recorded_at_ms: 1_700_000_030_001,
        },
        JournalEntry {
            cursor: 4,
            event_id: EventId(4),
            event: RepositoryEvent::HeadChanged(HeadChangeReceipt {
                worktree: wt.clone(),
                // No prior baseline. This crate's own register() never
                // constructs this state (see monitor.rs's module docs), but
                // it is a valid wire state the struct's Option permits, and a
                // durable-format golden pins what the type CAN hold, not only
                // what today's code happens to produce.
                old_head: None,
                new_head: oid('6'),
                kind: HeadChangeKind::Rewound,
                branch: Some(BranchName::from_raw(
                    "tidepool/worktree/sample-wt-finalized-0002",
                )),
                observed_at_ms: 1_700_000_040_000,
            }),
            recorded_at_ms: 1_700_000_040_001,
        },
        JournalEntry {
            cursor: 5,
            event_id: EventId(5),
            event: RepositoryEvent::HeadChanged(HeadChangeReceipt {
                worktree: wt.clone(),
                old_head: Some(oid('6')),
                new_head: oid('b'),
                kind: HeadChangeKind::Switched,
                branch: Some(BranchName::from_raw("tidepool/worktree/other-branch")),
                observed_at_ms: 1_700_000_050_000,
            }),
            recorded_at_ms: 1_700_000_050_001,
        },
        JournalEntry {
            cursor: 6,
            event_id: EventId(6),
            event: RepositoryEvent::HeadChanged(HeadChangeReceipt {
                worktree: wt.clone(),
                old_head: Some(oid('b')),
                new_head: oid('c'),
                kind: HeadChangeKind::UnknownChange,
                branch: None,
                observed_at_ms: 1_700_000_060_000,
            }),
            recorded_at_ms: 1_700_000_060_001,
        },
        JournalEntry {
            cursor: 7,
            event_id: EventId(7),
            event: RepositoryEvent::Commit(CommitReceipt {
                worktree: wt.clone(),
                oid: oid('d'),
                parents: vec![],
                subject: "root commit".to_string(),
                author: "Scripted Writer <writer@example.test>".to_string(),
                committed_at_ms: 1_700_000_070_000,
                files: vec![],
            }),
            recorded_at_ms: 1_700_000_070_001,
        },
        JournalEntry {
            cursor: 8,
            event_id: EventId(8),
            event: RepositoryEvent::Commit(CommitReceipt {
                worktree: wt,
                oid: oid('e'),
                parents: vec![oid('d'), oid('f')],
                subject: "merge branch".to_string(),
                author: "Scripted Writer <writer@example.test>".to_string(),
                committed_at_ms: 1_700_000_080_000,
                files: vec!["a.rs".to_string(), "b.rs".to_string()],
            }),
            recorded_at_ms: 1_700_000_080_001,
        },
    ]
}

// --- Layer 1: whole-file golden compare + Layer 3: deserialize round-trip. -

#[test]
fn worktree_receipt_provisional_golden_round_trips() {
    let sample = receipt_provisional();
    let json = assert_golden("worktree_receipt_provisional.json", &sample);
    let back: WorktreeReceipt = serde_json::from_str(&json).unwrap_or_else(|e| {
        panic!("WorktreeReceipt failed to deserialize from worktree_receipt_provisional.json: {e}")
    });
    assert_eq!(
        back, sample,
        "WorktreeReceipt round-trip through worktree_receipt_provisional.json \
         produced a different value than the original sample — a Deserialize-side \
         break (renamed field, changed default) would show up here even when \
         serialization alone still looks fine"
    );
}

#[test]
fn worktree_receipt_finalized_ref_origin_golden_round_trips() {
    let sample = receipt_finalized_ref_origin();
    let json = assert_golden("worktree_receipt_finalized_ref_origin.json", &sample);
    let back: WorktreeReceipt = serde_json::from_str(&json).unwrap_or_else(|e| {
        panic!(
            "WorktreeReceipt failed to deserialize from worktree_receipt_finalized_ref_origin.json: {e}"
        )
    });
    assert_eq!(
        back, sample,
        "WorktreeReceipt round-trip through worktree_receipt_finalized_ref_origin.json \
         produced a different value than the original sample"
    );
}

#[test]
fn worktree_receipt_finalized_worktree_origin_golden_round_trips() {
    let sample = receipt_finalized_worktree_origin();
    let json = assert_golden("worktree_receipt_finalized_worktree_origin.json", &sample);
    let back: WorktreeReceipt = serde_json::from_str(&json).unwrap_or_else(|e| {
        panic!(
            "WorktreeReceipt failed to deserialize from worktree_receipt_finalized_worktree_origin.json: {e}"
        )
    });
    assert_eq!(
        back, sample,
        "WorktreeReceipt round-trip through worktree_receipt_finalized_worktree_origin.json \
         produced a different value than the original sample"
    );
}

#[test]
fn binding_rows_golden_round_trips() {
    let sample = binding_rows();
    let json = assert_golden("binding_rows.json", &sample);
    let back: Vec<Binding> = serde_json::from_str(&json).unwrap_or_else(|e| {
        panic!("Vec<Binding> failed to deserialize from binding_rows.json: {e}")
    });
    assert_eq!(
        back, sample,
        "Vec<Binding> round-trip through binding_rows.json produced a different \
         value than the original sample"
    );
}

#[test]
fn journal_entries_golden_round_trips() {
    let sample = journal_entries();
    let json = assert_golden("journal_entries.json", &sample);
    let back: Vec<JournalEntry> = serde_json::from_str(&json).unwrap_or_else(|e| {
        panic!("Vec<JournalEntry> failed to deserialize from journal_entries.json: {e}")
    });
    assert_eq!(
        back, sample,
        "Vec<JournalEntry> round-trip through journal_entries.json produced a \
         different value than the original sample"
    );
}

// --- Layer 2: independent hardcoded pins. A blind regen (assert_golden's
// --- "file missing, write it" path) cannot launder a field-name or
// --- variant-tag change past these — they are typed directly into the test
// --- source, never read from a golden file. ---------------------------------

#[test]
fn worktree_receipt_field_shape_is_pinned_inline() {
    let sample = WorktreeReceipt {
        worktree_id: WorktreeId::from_raw("wt-pin"),
        cwd: PathBuf::from("/tmp/wt-pin"),
        branch: BranchName::from_raw("tidepool/worktree/pin"),
        source_head: oid('f'),
        snapshot_ref: None,
        origin: WorktreeOrigin::CurrentRepository,
        source_repository: PathBuf::from("/tmp/repo"),
        created_at_ms: 1,
        status: WorktreeRecordStatus::Finalized,
    };
    let json = serde_json::to_string(&sample)
        .unwrap_or_else(|e| panic!("failed to serialize the inline WorktreeReceipt pin: {e}"));
    assert_eq!(
        json,
        r#"{"worktree_id":"wt-pin","cwd":"/tmp/wt-pin","branch":"tidepool/worktree/pin","source_head":"ffffffffffffffffffffffffffffffffffffffff","snapshot_ref":null,"origin":"CurrentRepository","source_repository":"/tmp/repo","created_at_ms":1,"status":"Finalized"}"#,
        "WorktreeReceipt's serde shape (field names, field order, or the \
         WorktreeOrigin/WorktreeRecordStatus variant tags) drifted — this is an \
         inline literal, independent of tests/goldens/durable/, so no regen \
         command can launder this away. The type is durable JSON on operator \
         machines (tidepool-worktree/src/registry.rs); if this change is \
         deliberate, update both this literal and the golden files under \
         tests/goldens/durable/ in the same reviewed commit"
    );
}

#[test]
fn binding_field_shape_is_pinned_inline() {
    let sample = binding_row(
        WorktreeId::from_raw("wt-pin"),
        AgentRef::from_raw("agent-pin"),
        BindingState::Active,
        1,
    );
    let json = serde_json::to_string(&sample)
        .unwrap_or_else(|e| panic!("failed to serialize the inline Binding pin: {e}"));
    assert_eq!(
        json, r#"{"worktree":"wt-pin","agent":"agent-pin","state":"Active","bound_at_ms":1}"#,
        "Binding's serde shape (field names, field order, or the BindingState \
         variant tags) drifted — this is an inline literal, independent of \
         tests/goldens/durable/, so no regen command can launder this away. \
         The type is durable JSON on operator machines \
         (tidepool-worktree/src/binding.rs); if this change is deliberate, \
         update both this literal and the golden files under \
         tests/goldens/durable/ in the same reviewed commit"
    );
}

#[test]
fn worktree_error_field_shape_is_pinned_inline() {
    // WorktreeError is NOT written to disk by this crate (see the module doc
    // comment) — but §11.7 of the phase-3 scaffold names it explicitly among
    // the durable-format proof obligation, because its JSON shape is part of
    // the effect-error contract that tidepool-handlers' generated adapters
    // depend on. Pinned here, inline, rather than as a Class-D golden.
    let sample = WorktreeError::WorktreeBusy {
        worktree: WorktreeId::from_raw("wt-pin"),
        holder: "agent-pin".to_string(),
    };
    let json = serde_json::to_string(&sample)
        .unwrap_or_else(|e| panic!("failed to serialize the inline WorktreeError pin: {e}"));
    assert_eq!(
        json, r#"{"WorktreeBusy":{"worktree":"wt-pin","holder":"agent-pin"}}"#,
        "WorktreeError's serde shape (variant tag or field names/order) \
         drifted — this is an inline literal with no golden file behind it \
         at all, so no regen command can launder this away. WorktreeError is \
         not itself durable JSON (tidepool-worktree/src/error.rs — see this \
         file's module doc comment), but its shape is pinned because it is \
         part of the effect-error contract other crates depend on"
    );
}
