//! Single source of truth for the six bridged Rust↔Haskell wire records.
//!
//! Each type here derives `CoreRecord` (its Haskell `data` decl is generated
//! from the Rust struct — see `tidepool_bridge::CoreRecord`) plus `ToCore`,
//! so it can be handed straight to `EffectContext::respond`. Living in a LOW
//! crate (depends only on `tidepool-bridge`/`tidepool-eval`/`tidepool-repr`)
//! means both the real handlers (`tidepool-handlers`, a TOP crate) and test
//! mocks (`tidepool-testing`, `tidepool-runtime/tests`, both LOW crates) can
//! import the same struct without a dependency cycle — so a mock can no
//! longer hand-build a stale wire shape for one of these effect results.

#![warn(clippy::unwrap_used, clippy::expect_used)]
use tidepool_bridge_derive::{CoreRecord, ToCore};

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
#[derive(ToCore, Clone, CoreRecord)]
pub struct Proc {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

/// Haskell `Hit` record: path / line / text — a search match (`grepGlob` and
/// the shared structural-search surface).
#[derive(
    tidepool_bridge_derive::ToCore,
    tidepool_bridge_derive::FromCore,
    Clone,
    Debug,
    PartialEq,
    CoreRecord,
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
    tidepool_bridge_derive::ToCore,
    tidepool_bridge_derive::FromCore,
    Clone,
    Debug,
    PartialEq,
    CoreRecord,
)]
pub struct FileMeta {
    pub size: i64,
    pub is_file: bool,
    pub is_dir: bool,
}

/// Haskell `Commit` record: sha / subject / author / date / files.
#[derive(ToCore, Clone, CoreRecord)]
#[core(name = "Commit")]
pub struct GitCommit {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub date: String,
    pub files: Vec<String>,
}

/// Haskell `StatusEntry` record: path / state (2-char XY code).
#[derive(ToCore, Clone, CoreRecord)]
#[core(name = "StatusEntry")]
pub struct GitStatusEntry {
    pub path: String,
    pub state: String,
}

/// Haskell `FileDelta` record: path / adds / dels / binary.
#[derive(ToCore, Clone, CoreRecord)]
#[core(name = "FileDelta")]
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
#[derive(ToCore, Clone, CoreRecord)]
#[core(name = "CommitDeltas")]
pub struct GitCommitDeltas {
    #[core(hs_type = "Commit")]
    pub commit: GitCommit,
    #[core(hs_type = "[FileDelta]")]
    pub deltas: Vec<GitFileDelta>,
}

use tidepool_bridge_derive::FromCore;

// ============================================================================
// Subagent wire types (PRD 18 lane 1 — coupled spawn), `Ag*`-prefixed.
//
// THE `Wt*`/`Ev*` FAMILIES NO LONGER LIVE HERE — both are generated into
// `src/generated/` from `tidepool-protocol` (PRD 22 phase 3). `Ag*` below
// still does NOT derive `CoreRecord` (its Haskell decls are single-sourced
// from `subagent_effect_def!`'s `type_defs`), and field ORDER in each struct
// is the wire contract, matching those decls positionally. PROVISIONAL
// shapes — this lane exists to inform PRD 18's freezes, and renames land
// here + in the
// effect def together.
// ============================================================================

/// Haskell `AgentId` — Tidepool's identity for one agent, minted by the
/// runtime, never by a backend.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]
#[core(name = "AgentId")]
pub struct AgAgentId {
    pub raw: i64,
}

/// Haskell `CycleId` — the handler-scoped identity of one running cycle.
/// Opaque: Tidepool mints it, echoes it, and never parses it. Cycle-scoped
/// like every PRD 19 handle — it never crosses a resident-cycle boundary.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]
#[core(name = "CycleId")]
pub struct AgCycleId {
    pub raw: i64,
}

/// Haskell `BackendThreadId` — a backend's identity for the hosting thread.
/// Opaque: stored, compared, echoed, never parsed.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "BackendThreadId")]
pub struct AgBackendThreadId {
    pub raw: String,
}

/// Haskell `SpawnWorkspace` — a new managed worktree, or an existing UNBOUND
/// one by durable id (PRD 18 addendum decision 3/4: coupled-only surface).
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
pub enum AgSpawnWorkspace {
    SpawnNewWorktree(WtWorktreeSpec),
    SpawnExistingWorktree(WtWorktreeId),
}

/// Haskell `SpawnSpec` — built in Haskell, consumed in Rust. The result
/// schema rides the verb's separate `Value` argument (JsonArg lane), not this
/// record.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "SpawnSpec")]
pub struct AgSpawnSpec {
    pub spawn_workspace: AgSpawnWorkspace,
    pub spawn_agent_label: String,
    pub spawn_task: String,
}

/// Haskell `SpawnStage` — how far the saga got; carried on every SpawnError.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgSpawnStage {
    StageAllocating,
    StageWorktreeReady,
    StageBound,
    StageThreadAccepted,
    StageRunning,
}

/// Haskell `BackendFailure` — the seam's `AgentBackendError`, case-matchable
/// (retryable-vs-not, mine-vs-theirs).
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
pub enum AgBackendFailure {
    BackendUnavailable(String),
    ProtocolRejected(String),
    RunFailed(String),
}

/// Haskell `CyclePayload` — what the terminal message actually was.
/// `PayloadStructured` is not yet a typed success: decoding against the
/// caller's type happens Haskell-side, and a decode failure there is a typed
/// error. ToCore-only: this is outbound (`serde_json::Value` has no FromCore).
#[derive(ToCore, Clone, Debug, PartialEq)]
pub enum AgCyclePayload {
    PayloadStructured(serde_json::Value),
    PayloadUnstructured(String),
    PayloadAbsent,
}

/// Haskell `AgentActivity` — receipt-bearing observations from the run. Model
/// prose is never the source of any field (PRD 18: "runtime receipts are
/// authoritative"), so `ActivityCommand`'s exit code is what the process
/// returned, and `Nothing` is "the backend reported no exit code" (killed by a
/// signal, or still the only thing it said), never "it succeeded".
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
pub enum AgAgentActivity {
    ActivityCommand(String, Option<i64>),
    ActivityFileChanged(String),
}

/// Haskell `TokenUsage` — what one turn actually cost, as the backend reported
/// it. Every counter is present or the whole record is absent
/// (`AgSpawnReceipt::receipt_usage`'s `Option`): a backend that reports usage
/// reports all of it.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]
#[core(name = "TokenUsage")]
pub struct AgTokenUsage {
    pub usage_input: i64,
    pub usage_cached_input: i64,
    pub usage_output: i64,
    pub usage_reasoning_output: i64,
    pub usage_total: i64,
}

/// Haskell `WorkerRun` — the coupled pair one spawn yields (PRD 19's result
/// shape) plus the backend thread identity.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "WorkerRun")]
pub struct AgWorkerRun {
    pub run_agent: AgAgentId,
    pub run_worktree: WtWorktreeHandle,
    pub run_thread: AgBackendThreadId,
}

/// Haskell `SpawnReceipt` — every field checkable against disk or the
/// backend; `receipt_model` is the EXACT resolved model, never a tier name.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "SpawnReceipt")]
pub struct AgSpawnReceipt {
    pub receipt_agent: AgAgentId,
    pub receipt_worktree: WtWorktreeId,
    pub receipt_binding_ref: String,
    pub receipt_thread: AgBackendThreadId,
    pub receipt_model: String,
    pub receipt_turn: String,
    /// How many tool-call rounds the child actually took — checkable against
    /// the backend's own transcript, and the number a budget conversation
    /// needs. APPENDED after the lane-1 fields: field order is the wire
    /// contract.
    pub receipt_rounds: i64,
    /// `None` is "the backend said nothing about usage", never "it was free".
    pub receipt_usage: Option<AgTokenUsage>,
}

/// Haskell `SpawnOutcome` — the verb's success payload. ToCore-only (carries
/// `AgCyclePayload`).
#[derive(ToCore, Clone, Debug, PartialEq)]
#[core(name = "SpawnOutcome")]
pub struct AgSpawnOutcome {
    pub outcome_run: AgWorkerRun,
    pub outcome_payload: AgCyclePayload,
    pub outcome_receipt: AgSpawnReceipt,
    /// What the run actually did, in the order the backend reported it.
    /// APPENDED, per the same rule as `AgSpawnReceipt`'s new tail.
    pub outcome_activity: Vec<AgAgentActivity>,
}

/// Haskell `AgentStep` — where a driven spawn stopped. The authored loop
/// alternates: a `StepToolCall` is answered and driving continues, a
/// `StepDone` ends the agent's life.
///
/// `StepToolCall`'s fields are positional — agent, call id, tool name,
/// arguments — matching `AgCyclePayload`'s style for a sum carrying a `Value`.
/// ToCore-only: this is OUTBOUND, and `serde_json::Value` has no `FromCore`
/// (which is also why the inbound tool declarations and tool answer ride flat
/// `Value` verb arguments through `JsonArg` instead of a bridged record).
/// The outcome is BOXED — transparently, since `ToCore for Box<T>` delegates
/// to `T`, so the wire shape is still `StepDone SpawnOutcome`. Same reason the
/// domain's `SpawnStep::Done` boxes: a whole outcome inside a two-variant enum
/// makes every parked call pay for the finished one.
#[derive(ToCore, Clone, Debug, PartialEq)]
pub enum AgAgentStep {
    StepToolCall(AgAgentId, String, String, serde_json::Value),
    StepDone(Box<AgSpawnOutcome>),
}

/// Build the `Tidepool.Records.Bridged` Haskell module — the GENERATED home of
/// the fully-migrated bridged result records, where the Rust struct is the
/// single source of truth. Materialized to the committed
/// `haskell/lib/Tidepool/Records/Bridged.hs` (kept in sync by the
/// `bridged_records` test) and re-exported by `Tidepool.Records` →
/// `Tidepool.Prelude`.
///
/// Field ORDER is the wire contract: `ToCore` builds the `Con` in Rust struct
/// field order; the extract assigns positions from this decl's field order.
pub fn bridged_records_module() -> String {
    use tidepool_bridge::CoreRecord;
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
    out.push_str("-- (each carries `#[derive(CoreRecord)]`). DO NOT EDIT BY HAND: the Rust\n");
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
