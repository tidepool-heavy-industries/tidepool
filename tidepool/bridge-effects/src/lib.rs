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

impl Model {
    /// Frozen workspace key for configured tiers and custom aliases.
    #[must_use]
    pub fn workspace_key(&self) -> Option<&str> {
        match self {
            Self::Luna => Some("luna"),
            Self::Sol => Some("sol"),
            Self::Astra => Some("astra"),
            Self::CustomAlias(alias) => Some(alias),
            Self::ProviderModel(_) => None,
        }
    }

    /// Requested selector text for presentation; configuration owns resolution.
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::ProviderModel(model) => model,
            _ => self.workspace_key().expect("configured selector"),
        }
    }
}

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

#[cfg(test)]
mod context_constructor_tests {
    use super::{ContextBlock, ContextNativeKind, ContextRole, Model};
    use tidepool_bridge::{FromHaskell, HaskellValue, ToHaskell};
    use tidepool_repr::{DataCon, DataConId, DataConTable};

    fn row(id: u64, module: &str, name: &str, arity: u32) -> DataCon {
        DataCon {
            identity: tidepool_repr::execution_schema::testing::metadata_identity(
                tidepool_repr::execution_schema::SymbolIdentity {
                    unit: "fixture".into(),
                    module: module.into(),
                    namespace: "constructor".into(),
                    occurrence: name.into(),
                    record_parent: None,
                },
                "FixtureType",
            ),
            id: DataConId(id),
            name: name.into(),
            tag: 1,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: Some(format!("{module}.{name}")),
            type_name: "FixtureType".into(),
        }
    }

    #[test]
    fn model_selectors_roundtrip_preserves_tiers_custom_alias_and_provider_identity() {
        let mut table = DataConTable::new();
        table
            .extend_checked([
                row(1, "Tidepool.Effects.Core", "Luna", 0),
                row(2, "Tidepool.Effects.Core", "Sol", 0),
                row(3, "Tidepool.Effects.Core", "Astra", 0),
                row(4, "Tidepool.Effects.Core", "CustomAlias", 1),
                row(5, "Tidepool.Effects.Core", "ProviderModel", 1),
                row(6, "Data.Text", "Text", 3),
                row(7, "User", "Luna", 0),
            ])
            .unwrap();
        for model in [
            Model::Luna,
            Model::Sol,
            Model::Astra,
            Model::CustomAlias("luna".into()),
            Model::CustomAlias("unknown-custom".into()),
            Model::ProviderModel("luna".into()),
        ] {
            let encoded = model.to_value(&table).unwrap();
            assert_eq!(Model::from_value(&encoded, &table).unwrap(), model);
        }
        assert!(Model::from_value(&HaskellValue::Con(DataConId(7), vec![]), &table).is_err());
        assert!(Model::from_value(
            &HaskellValue::Con(DataConId(1), vec![HaskellValue::Con(DataConId(2), vec![]),]),
            &table
        )
        .is_err());
    }

    #[test]
    fn context_text_constructor_preserves_module_identity_with_internal_text_present() {
        let mut table = DataConTable::new();
        table
            .extend_checked([
                row(1, "Tidepool.Effects.Core", "Text", 4),
                row(2, "Data.Text.Internal", "Text", 3),
                row(3, "GHC.Maybe", "Nothing", 0),
                row(4, "Tidepool.Effects.Core", "User", 0),
                row(5, "GHC.Types", "[]", 0),
                row(7, "GHC.Types", ":", 2),
            ])
            .unwrap();
        // String encoding uses the public Text alias, while ContextBlock uses its defining module.
        let mut internal = table.get(DataConId(2)).unwrap().clone();
        internal.qualified_name = Some("Data.Text.Text".into());
        table.insert_checked(internal).unwrap();
        let block = ContextBlock::Text {
            reference: None,
            role: ContextRole::User,
            text: "retained persona".into(),
            sources: vec![],
        };
        let value = block.to_value(&table).unwrap();
        let HaskellValue::Con(id, fields) = &value else {
            panic!("expected context constructor")
        };
        assert_eq!(*id, DataConId(1));
        assert!(matches!(&fields[2], HaskellValue::Con(id, _) if *id == DataConId(2)));
        assert_eq!(ContextBlock::from_value(&value, &table).unwrap(), block);
        // A second exact owner at the qualified spelling must still refuse selection.
        let mut foreign = table.get(DataConId(1)).unwrap().clone();
        foreign.id = DataConId(6);
        foreign.identity.symbol.unit = "foreign-unit".into();
        foreign.identity.family.symbol.unit = "foreign-unit".into();
        table.insert_checked(foreign).unwrap();
        assert!(matches!(
            block.to_value(&table),
            Err(tidepool_bridge::BridgeError::AmbiguousDataConNameArity { .. })
        ));
    }

    #[test]
    fn context_pending_constructor_preserves_module_identity_with_exit_cell_present() {
        let mut table = DataConTable::new();
        table
            .extend_checked([
                row(1, "Tidepool.Effects.Core", "Pending", 0),
                row(2, "Tidepool.Internal.ExitCell", "Pending", 1),
            ])
            .unwrap();
        let value = ContextNativeKind::Pending.to_value(&table).unwrap();
        assert!(
            matches!(&value, HaskellValue::Con(id, fields) if *id == DataConId(1) && fields.is_empty())
        );
        assert_eq!(
            ContextNativeKind::from_value(&value, &table).unwrap(),
            ContextNativeKind::Pending
        );
    }
}
