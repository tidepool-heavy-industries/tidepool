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

impl Backend {
    #[must_use]
    pub fn config_name(&self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenAIPlan => "chatgpt_plan",
            Self::GeminiInteractions => "gemini_interactions",
        }
    }

    pub fn from_config_name(name: &str) -> Result<Self, String> {
        match name {
            "deepseek" => Ok(Self::DeepSeek),
            "chatgpt_plan" => Ok(Self::OpenAIPlan),
            "gemini_interactions" => Ok(Self::GeminiInteractions),
            "codex" => Err(
                "private Codex credential bridge has been retired; select deepseek, chatgpt_plan or gemini_interactions"
                    .into(),
            ),
            _ => Err(format!(
                "unknown inference backend {name:?}; select deepseek, chatgpt_plan or gemini_interactions"
            )),
        }
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}/{}",
            self.backend().config_name(),
            self.value()
        )
    }
}

impl std::fmt::Display for Reasoning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}/{}",
            self.backend().config_name(),
            self.value()
        )
    }
}

impl Model {
    /// Model identifiers are interpreted only within an explicitly selected backend.
    #[must_use]
    pub fn for_backend(backend: Backend, id: &str) -> Self {
        match (&backend, id) {
            (Backend::DeepSeek, "deepseek-flash") => Self::DeepSeekFlash,
            (Backend::GeminiInteractions, "gemini-4-argon") => Self::GeminiArgon,
            (Backend::DeepSeek, "deepseek-v4-pro") => Self::DeepSeekPro,
            (Backend::OpenAIPlan, "gpt-6.1-sol") => Self::OpenAIModel(OpenAIModel::Gpt61Sol),
            (Backend::OpenAIPlan, "gpt-6-sol") => Self::OpenAIModel(OpenAIModel::Gpt6Sol),
            (Backend::OpenAIPlan, "gpt-6-astra") => Self::OpenAIModel(OpenAIModel::Gpt6Astra),
            (Backend::OpenAIPlan, "gpt-6-luna") => Self::OpenAIModel(OpenAIModel::Gpt6Luna),
            (Backend::OpenAIPlan, "gpt-5.6-sol") => Self::OpenAIModel(OpenAIModel::Gpt56Sol),
            _ => Self::CustomModel(backend, id.to_owned()),
        }
    }

    /// Provider ownership is explicit; model spelling never selects a route.
    #[must_use]
    pub fn backend(&self) -> Backend {
        match self {
            Self::DeepSeekFlash | Self::DeepSeekPro => Backend::DeepSeek,
            Self::OpenAIModel(_) => Backend::OpenAIPlan,
            Self::GeminiArgon => Backend::GeminiInteractions,
            Self::CustomModel(backend, _) => backend.clone(),
        }
    }

    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::DeepSeekFlash => "deepseek-flash",
            Self::GeminiArgon => "gemini-4-argon",
            Self::DeepSeekPro => "deepseek-v4-pro",
            Self::OpenAIModel(model) => model.value(),
            Self::CustomModel(_, model) => model,
        }
    }
}

impl OpenAIModel {
    #[must_use]
    pub fn value(&self) -> &'static str {
        match self {
            Self::Gpt61Sol => "gpt-6.1-sol",
            Self::Gpt6Sol => "gpt-6-sol",
            Self::Gpt6Astra => "gpt-6-astra",
            Self::Gpt6Luna => "gpt-6-luna",
            Self::Gpt56Sol => "gpt-5.6-sol",
        }
    }
}

impl Reasoning {
    pub fn for_backend(backend: Backend, effort: &str) -> Result<Self, String> {
        match (backend, effort) {
            (Backend::DeepSeek, "none") => {
                Ok(Self::DeepSeekReasoning(DeepSeekReasoning::DeepSeekNone))
            }
            (Backend::DeepSeek, "low") => {
                Ok(Self::DeepSeekReasoning(DeepSeekReasoning::DeepSeekLow))
            }
            (Backend::DeepSeek, "high") => {
                Ok(Self::DeepSeekReasoning(DeepSeekReasoning::DeepSeekHigh))
            }
            (Backend::DeepSeek, "max") => {
                Ok(Self::DeepSeekReasoning(DeepSeekReasoning::DeepSeekMax))
            }
            (Backend::OpenAIPlan, "low") => Ok(Self::OpenAIReasoning(OpenAIReasoning::OpenAILow)),
            (Backend::OpenAIPlan, "medium") => {
                Ok(Self::OpenAIReasoning(OpenAIReasoning::OpenAIMedium))
            }
            (Backend::OpenAIPlan, "high") => Ok(Self::OpenAIReasoning(OpenAIReasoning::OpenAIHigh)),
            (Backend::GeminiInteractions, "low") => {
                Ok(Self::ArgonReasoning(ArgonEffort::EffortLow))
            }
            (Backend::GeminiInteractions, "medium") => {
                Ok(Self::ArgonReasoning(ArgonEffort::EffortMedium))
            }
            (Backend::GeminiInteractions, "high") => {
                Ok(Self::ArgonReasoning(ArgonEffort::EffortHigh))
            }
            _ => Err("reasoning setting is not supported by the selected backend".into()),
        }
    }

    #[must_use]
    pub fn value(&self) -> &'static str {
        match self {
            Self::DeepSeekReasoning(DeepSeekReasoning::DeepSeekNone) => "none",
            Self::DeepSeekReasoning(DeepSeekReasoning::DeepSeekLow)
            | Self::OpenAIReasoning(OpenAIReasoning::OpenAILow)
            | Self::ArgonReasoning(ArgonEffort::EffortLow) => "low",
            Self::OpenAIReasoning(OpenAIReasoning::OpenAIMedium)
            | Self::ArgonReasoning(ArgonEffort::EffortMedium) => "medium",
            Self::DeepSeekReasoning(DeepSeekReasoning::DeepSeekHigh)
            | Self::OpenAIReasoning(OpenAIReasoning::OpenAIHigh)
            | Self::ArgonReasoning(ArgonEffort::EffortHigh) => "high",
            Self::DeepSeekReasoning(DeepSeekReasoning::DeepSeekMax) => "max",
        }
    }

    #[must_use]
    pub fn backend(&self) -> Backend {
        match self {
            Self::DeepSeekReasoning(_) => Backend::DeepSeek,
            Self::OpenAIReasoning(_) => Backend::OpenAIPlan,
            Self::ArgonReasoning(_) => Backend::GeminiInteractions,
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
    use super::{
        ArgonEffort, Backend, ContextBlock, ContextNativeKind, ContextRole, Model, OpenAIModel,
        Reasoning,
    };
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
    fn reload_options_and_typed_receipts_roundtrip_and_reject_wrong_arity() {
        use super::{
            RlOptions, RlPolicy, RlRefusal, RlRefusalCategory, RlSurfaceDiff, SrReloadOutcome,
            SrRevision,
        };
        let mut table = DataConTable::new();
        table
            .extend_checked([
                row(1, "Tidepool.Effects.Core", "ReloadOptions", 2),
                row(2, "Tidepool.Effects.Core", "CompatibleOnly", 0),
                row(3, "Tidepool.Effects.Core", "AcceptCacheInvalidation", 0),
                row(4, "Tidepool.Effects.Core", "ReloadSurfaceDiff", 3),
                row(5, "Tidepool.Effects.Core", "ReloadRefusal", 3),
                row(6, "Tidepool.Effects.Core", "SurfaceIncompatible", 0),
                row(7, "Tidepool.Effects.Core", "SourceRevision", 3),
                row(8, "Tidepool.Effects.Core", "ReloadPublished", 4),
                row(9, "Tidepool.Effects.Core", "ReloadRefused", 3),
                row(10, "GHC.Types", "[]", 0),
                row(11, "GHC.Types", ":", 2),
                row(12, "Data.Text", "Text", 3),
                row(13, "GHC.Types", "I#", 1),
            ])
            .unwrap();
        for policy in [RlPolicy::CompatibleOnly, RlPolicy::AcceptCacheInvalidation] {
            let options = RlOptions {
                also_check: vec!["Additional".into()],
                policy,
            };
            let encoded = options.to_value(&table).unwrap();
            assert_eq!(RlOptions::from_value(&encoded, &table).unwrap(), options);
        }
        let revision = SrRevision {
            identity: "exact-content".into(),
            generation: 0,
            modules: vec![],
        };
        let diff = RlSurfaceDiff {
            added: vec!["new".into()],
            removed: vec![],
            changed: vec!["changed".into()],
        };
        for receipt in [
            SrReloadOutcome::ReloadPublished(
                revision.clone(),
                revision.clone(),
                vec![],
                diff.clone(),
            ),
            SrReloadOutcome::ReloadRefused(
                revision.clone(),
                revision,
                RlRefusal {
                    category: RlRefusalCategory::SurfaceIncompatible,
                    detail: "supported surface changed".into(),
                    surface: diff,
                },
            ),
        ] {
            let encoded = receipt.to_value(&table).unwrap();
            assert_eq!(
                SrReloadOutcome::from_value(&encoded, &table).unwrap(),
                receipt
            );
        }
        assert!(RlOptions::from_value(&HaskellValue::Con(DataConId(1), vec![]), &table).is_err());
        assert!(
            SrReloadOutcome::from_value(&HaskellValue::Con(DataConId(8), vec![]), &table).is_err()
        );
    }

    #[test]
    fn model_selectors_roundtrip_preserve_explicit_backend_identity() {
        let mut table = DataConTable::new();
        table
            .extend_checked([
                row(1, "Tidepool.Effects.Core", "DeepSeekFlash", 0),
                row(2, "Tidepool.Effects.Core", "DeepSeekPro", 0),
                row(3, "Tidepool.Effects.Core", "OpenAIModel", 1),
                row(4, "Tidepool.Effects.Core", "CustomModel", 2),
                row(5, "Tidepool.Effects.Core", "DeepSeek", 0),
                row(6, "Tidepool.Effects.Core", "OpenAIPlan", 0),
                row(7, "Tidepool.Effects.Core", "Gpt61Sol", 0),
                row(8, "Data.Text", "Text", 3),
                row(9, "User", "DeepSeekFlash", 0),
                row(10, "Tidepool.Effects.Core", "GeminiArgon", 0),
                row(11, "Tidepool.Effects.Core", "GeminiInteractions", 0),
                row(12, "Tidepool.Effects.Core", "ArgonReasoning", 1),
                row(13, "Tidepool.Effects.Core", "EffortLow", 0),
                row(14, "Tidepool.Effects.Core", "EffortMedium", 0),
                row(15, "Tidepool.Effects.Core", "EffortHigh", 0),
            ])
            .unwrap();
        for model in [
            Model::DeepSeekFlash,
            Model::DeepSeekPro,
            Model::GeminiArgon,
            Model::CustomModel(Backend::GeminiInteractions, "custom".into()),
            Model::OpenAIModel(OpenAIModel::Gpt61Sol),
            Model::CustomModel(Backend::DeepSeek, "custom".into()),
            Model::CustomModel(Backend::OpenAIPlan, "custom".into()),
        ] {
            let encoded = model.to_value(&table).unwrap();
            assert_eq!(Model::from_value(&encoded, &table).unwrap(), model);
        }
        for effort in [
            ArgonEffort::EffortLow,
            ArgonEffort::EffortMedium,
            ArgonEffort::EffortHigh,
        ] {
            let reasoning = Reasoning::ArgonReasoning(effort);
            let encoded = reasoning.to_value(&table).unwrap();
            assert_eq!(Reasoning::from_value(&encoded, &table).unwrap(), reasoning);
        }
        assert!(Reasoning::from_value(&HaskellValue::Con(DataConId(12), vec![]), &table).is_err());
        assert!(Model::from_value(&HaskellValue::Con(DataConId(9), vec![]), &table).is_err());
        assert_ne!(
            Model::CustomModel(Backend::DeepSeek, "same".into()).backend(),
            Model::CustomModel(Backend::OpenAIPlan, "same".into()).backend()
        );
        assert!(Model::from_value(
            &HaskellValue::Con(DataConId(1), vec![HaskellValue::Con(DataConId(2), vec![])]),
            &table
        )
        .is_err());
    }

    #[test]
    fn argon_selectors_require_backend_and_strict_effort() {
        let backend = Backend::GeminiInteractions;
        assert_eq!(
            Backend::from_config_name("gemini_interactions").unwrap(),
            backend
        );
        assert_eq!(backend.config_name(), "gemini_interactions");
        assert_eq!(
            Model::for_backend(backend, "gemini-4-argon"),
            Model::GeminiArgon
        );
        assert_eq!(Model::GeminiArgon.value(), "gemini-4-argon");
        assert_eq!(Model::GeminiArgon.backend(), backend);
        assert_eq!(
            Model::GeminiArgon.to_string(),
            "gemini_interactions/gemini-4-argon"
        );
        for other in [Backend::DeepSeek, Backend::OpenAIPlan] {
            assert_eq!(
                Model::for_backend(other, "gemini-4-argon"),
                Model::CustomModel(other, "gemini-4-argon".into())
            );
        }
        assert_eq!(
            Model::for_backend(backend, "future-model"),
            Model::CustomModel(backend, "future-model".into())
        );
        for (effort, typed, wire) in [
            ("low", ArgonEffort::EffortLow, "EffortLow"),
            ("medium", ArgonEffort::EffortMedium, "EffortMedium"),
            ("high", ArgonEffort::EffortHigh, "EffortHigh"),
        ] {
            let reasoning = Reasoning::for_backend(backend, effort).unwrap();
            assert_eq!(reasoning, Reasoning::ArgonReasoning(typed));
            assert_eq!(reasoning.value(), effort);
            assert_eq!(reasoning.backend(), backend);
            let encoded = serde_json::to_value(reasoning).unwrap();
            assert_eq!(
                encoded,
                serde_json::json!({"tag":"ArgonReasoning", "contents": wire})
            );
            assert_eq!(
                serde_json::from_value::<Reasoning>(encoded).unwrap(),
                reasoning
            );
        }
        for invalid in ["none", "max", "ultra", "LOW", "", "1024"] {
            assert!(Reasoning::for_backend(backend, invalid).is_err());
            assert!(serde_json::from_value::<Reasoning>(
                serde_json::json!({"tag":"ArgonReasoning", "contents":invalid})
            )
            .is_err());
        }
        for invalid in [
            serde_json::json!({"tag":"ArgonReasoning", "contents":1024}),
            serde_json::json!({"tag":"ArgonReasoning", "contents":"EffortLow", "budget":1024}),
        ] {
            assert!(serde_json::from_value::<Reasoning>(invalid).is_err());
        }
        for model in [
            Model::GeminiArgon,
            Model::CustomModel(backend, "gemini-4-argon".into()),
        ] {
            assert_eq!(
                serde_json::from_value::<Model>(serde_json::to_value(&model).unwrap()).unwrap(),
                model
            );
        }
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
