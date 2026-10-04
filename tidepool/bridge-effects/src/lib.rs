//! Single source of truth for the six bridged Rust↔Haskell wire records.
//!
//! Each type here derives `HaskellRecord` (its Haskell `data` decl is generated
//! from the Rust struct — see `tidepool_bridge::HaskellRecord`) plus `ToHaskell`,
//! so it can be handed straight to `EffectContext::respond`. Living in a LOW
//! crate (depends only on `tidepool-bridge` and `tidepool-repr`)
//! means both the real handlers (`tidepool-handlers`, a TOP crate) and test
//! mocks (`tidepool-testing`, `tidepool/runtime/tests`, both LOW crates) can
//! import the same struct without a dependency cycle — so a mock can no
//! longer hand-build a stale wire shape for one of these effect results.

use tidepool_bridge_derive::{HaskellRecord, ToHaskell};

/// The GENERATED wire families. One ordered field list in `tidepool-protocol`
/// renders both the Haskell declaration and the Rust struct, so the positional
/// wire contract is true by construction rather than by comment. Re-exported
/// flat, matching this crate's existing surface — every
/// `tidepool_bridge_effects::Wt*` path resolves exactly as it did when those
/// types were hand-written below.
pub mod generated;
pub use generated::*;

/// Hand-written extension methods on the GENERATED `EvRepositoryEvent` — logic,
/// not contract, so it stays outside the schema (same split as an
/// `AdapterKind::HandWritten` conversion — see `tidepool-protocol`'s `DomainMap`
/// doc). Not derivable from the field list: `worktree()` and `matches()` both
/// encode a DECISION (which fact-kind carries which watch/worktree), not a
/// mechanical projection.
impl EvRepositoryEvent {
    /// The worktree this fact is about — what a subscription's watches match
    /// on. `None` for a `Tick`, an async-done, or a mailbox message: none of
    /// them are about any worktree.
    pub fn worktree(&self) -> Option<&WtWorktreeId> {
        match self {
            EvRepositoryEvent::ObservedCommit(_, r) => Some(&r.commit_worktree),
            EvRepositoryEvent::ObservedHeadChange(_, r) => Some(&r.head_worktree),
            EvRepositoryEvent::ObservedTick(_, _) => None,
            EvRepositoryEvent::ObservedAsyncDone(_, _) => None,
            EvRepositoryEvent::ObservedMessage(_, _, _) => None,
        }
    }

    /// Does `watch` select this fact? Kind AND identity must both match: a
    /// `commit` subscription on tree A must not be woken by a head movement,
    /// nor by tree B's commit; a `WatchMailbox` subscription on mailbox 1
    /// must not be woken by a message sent to mailbox 2. `Tick`/`WatchDeadline`
    /// never match here — they are queued directly by the registry's
    /// `fire_due_deadlines`, never through its broadcast `publish`.
    pub fn matches(&self, watch: &EvWatch) -> bool {
        match (self, watch) {
            (EvRepositoryEvent::ObservedCommit(_, r), EvWatch::WatchCommit(w)) => {
                &r.commit_worktree == w
            }
            (EvRepositoryEvent::ObservedHeadChange(_, r), EvWatch::WatchHead(w)) => {
                &r.head_worktree == w
            }
            (EvRepositoryEvent::ObservedAsyncDone(_, tid), EvWatch::WatchAsync(w)) => tid == w,
            (EvRepositoryEvent::ObservedMessage(_, mid, _), EvWatch::WatchMailbox(w)) => mid == w,
            _ => false,
        }
    }
}

/// Haskell `Proc` record: exitCode / stdout / stderr — a finished subprocess.
#[derive(ToHaskell, Clone, HaskellRecord)]
pub struct Proc {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

/// Haskell `Hit` record: path / line / text — a search match (`grepGlob` and
/// the shared structural-search surface).
#[derive(
    tidepool_bridge_derive::ToHaskell,
    tidepool_bridge_derive::FromHaskell,
    Clone,
    Debug,
    PartialEq,
    HaskellRecord,
)]
pub struct Hit {
    pub path: String,
    pub line: i64,
    pub text: String,
}

/// Haskell `FileMeta` record: size / isFile / isDir — filesystem metadata for
/// a path (`fsMeta`/`FsMetadata`). Absence of the path is `Nothing` at the
/// `Maybe FileMeta` level, not a field on this record.
#[derive(
    tidepool_bridge_derive::ToHaskell,
    tidepool_bridge_derive::FromHaskell,
    Clone,
    Debug,
    PartialEq,
    HaskellRecord,
)]
pub struct FileMeta {
    pub size: i64,
    pub is_file: bool,
    pub is_dir: bool,
}

/// Haskell `Commit` record: sha / subject / author / date / files.
#[derive(ToHaskell, Clone, HaskellRecord)]
#[haskell(name = "Commit")]
pub struct GitCommit {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub date: String,
    pub files: Vec<String>,
}

/// Haskell `StatusEntry` record: path / state (2-char XY code).
#[derive(ToHaskell, Clone, HaskellRecord)]
#[haskell(name = "StatusEntry")]
pub struct GitStatusEntry {
    pub path: String,
    pub state: String,
}

/// Haskell `FileDelta` record: path / adds / dels / binary.
#[derive(ToHaskell, Clone, HaskellRecord)]
#[haskell(name = "FileDelta")]
pub struct GitFileDelta {
    pub path: String,
    pub adds: i64,
    pub dels: i64,
    pub binary: bool,
}

/// Haskell `CommitDeltas` record: one commit paired with its own per-file
/// numstat deltas — the substrate `gitLogNumstat` returns in ONE subprocess,
/// where before a bulk git-history investigation needed `gitLog` (paths only)
/// plus a `mapM gitDiffStat` (one subprocess per commit) to assemble the same
/// shape by hand.
#[derive(ToHaskell, Clone, HaskellRecord)]
#[haskell(name = "CommitDeltas")]
pub struct GitCommitDeltas {
    #[haskell(hs_type = "Commit")]
    pub commit: GitCommit,
    #[haskell(hs_type = "[FileDelta]")]
    pub deltas: Vec<GitFileDelta>,
}

/// Build the `Tidepool.Records.Bridged` Haskell module — the GENERATED home of
/// the fully-migrated bridged result records, where the Rust struct is the
/// single source of truth. Materialized to the committed
/// `bridge/haskell/lib/Tidepool/Records/Bridged.hs` (kept in sync by the
/// `bridged_records` test) and re-exported by `Tidepool.Records` →
/// `Tidepool.Prelude`.
///
/// Field ORDER is the wire contract: `ToHaskell` builds the `Con` in Rust struct
/// field order; the extract assigns positions from this decl's field order.
pub fn bridged_records_module() -> String {
    use tidepool_bridge::HaskellRecord;
    let decls = [
        GitCommit::haskell_decl(),
        GitStatusEntry::haskell_decl(),
        GitFileDelta::haskell_decl(),
        GitCommitDeltas::haskell_decl(),
        Proc::haskell_decl(),
        Hit::haskell_decl(),
        FileMeta::haskell_decl(),
    ];
    let exports = decls
        .iter()
        .map(|d| format!("{}(..)", d.split_whitespace().nth(1).unwrap_or("")))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = String::new();
    out.push_str("{-# LANGUAGE NoImplicitPrelude, DuplicateRecordFields, NoFieldSelectors #-}\n\n");
    out.push_str(
        "-- | GENERATED from the Rust bridged-record structs in tidepool-bridge-effects\n",
    );
    out.push_str("-- (each carries `#[derive(HaskellRecord)]`). DO NOT EDIT BY HAND: the Rust\n");
    out.push_str("-- struct is the single source of truth for field order / name / type, and\n");
    out.push_str("-- this file is regenerated + verified by the `bridged_records` test\n");
    out.push_str(
        "-- (`TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records`).\n",
    );
    out.push_str("-- NoFieldSelectors: no field of any record here is exported as a top-level\n");
    out.push_str("-- function — access is record-dot only (HasField). Prevents a field name\n");
    out.push_str("-- (e.g. CommitDeltas's `commit`) from colliding with an unrelated binding\n");
    out.push_str("-- of the same name elsewhere (e.g. Tidepool.Event's `commit` builder).\n");
    out.push_str(&format!(
        "module Tidepool.Records.Bridged\n  ( {exports} ) where\n\n"
    ));
    out.push_str("import Prelude (Int, Bool, Eq, Show)\n");
    out.push_str("import Data.Text (Text)\n\n");
    for d in &decls {
        out.push_str(d);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod model_control_tests;
