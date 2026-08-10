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

use tidepool_bridge_derive::{CoreRecord, ToCore};

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

// ============================================================================
// PRD 19 — managed worktrees and typed repository events (lane L4)
// ============================================================================
//
// These are WIRE types, deliberately distinct from `tidepool-worktree`'s domain
// types of the same shape. The domain types carry `PathBuf`, `Option`, and the
// crate's own newtypes; the wire types carry exactly what crosses to Haskell,
// in the field order the generated `Tidepool.Effects` decl declares. Keeping
// them separate means `tidepool-bridge-effects` does not have to depend on
// `tidepool-worktree` (it is a LOW crate — see this module's header), and the
// handler does one explicit conversion instead of the bridge silently tracking
// a domain type's evolution.
//
// Unlike the six records above, these do NOT derive `CoreRecord`: their Haskell
// decls are single-sourced from `worktree_effect_def!` / `event_effect_def!`'s
// `type_defs` (which is also where the ADTs, the `Event` description type, and
// the `withHandler` interposition live), so generating a competing decl here
// would give the two copies room to disagree. Field ORDER in these structs is
// the wire contract and must match those `type_defs` decls positionally.

use tidepool_bridge_derive::FromCore;

/// Haskell `WorktreeId` — opaque durable identity. `data`, not a synonym: PRD
/// 19 requires that a `GitOid` can never be passed where a worktree id is
/// wanted.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "WorktreeId")]
pub struct WtWorktreeId {
    pub raw: String,
}

/// Haskell `GitOid` — domain data, distinct from `EvEventId`'s runtime identity.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "GitOid")]
pub struct WtGitOid {
    pub raw: String,
}

/// Haskell `GitRef` — a branch, tag, remote ref, or raw OID, resolved by git.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "GitRef")]
pub struct WtGitRef {
    pub raw: String,
}

/// Haskell `BranchName` — stored without the `refs/heads/` prefix.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "BranchName")]
pub struct WtBranchName {
    pub raw: String,
}

/// Haskell `WorktreeSource`.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
pub enum WtWorktreeSource {
    SourceCurrentRepository,
    SourceRef(WtGitRef),
    SourceWorktree(WtWorktreeId),
}

/// Haskell `DirtyPolicy`. Clean-by-default is the safety property; the opt-in
/// is spelled at the authored call site.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WtDirtyPolicy {
    RequireClean,
    AllowDirtySnapshot,
}

/// Haskell `WorktreeSpec` — built in Haskell, consumed in Rust.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "WorktreeSpec")]
pub struct WtWorktreeSpec {
    pub spec_source: WtWorktreeSource,
    pub spec_label: String,
    pub spec_dirty_policy: WtDirtyPolicy,
}

/// Haskell `InProgressKind` — distinguished rather than collapsed to a string
/// so a resident can branch on it.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WtInProgressKind {
    InProgressMerge,
    InProgressRebase,
    InProgressCherryPick,
    InProgressRevert,
    InProgressBisect,
}

/// Haskell `DirtySummary`. `ignored_excluded` is a COUNT, not a list: ignored
/// files are deliberately excluded from a snapshot, and listing them invites an
/// author to believe they were captured.
#[derive(ToCore, FromCore, Clone, Debug, Default, PartialEq, Eq)]
#[core(name = "DirtySummary")]
pub struct WtDirtySummary {
    pub staged: Vec<String>,
    pub unstaged: Vec<String>,
    pub untracked: Vec<String>,
    pub ignored_excluded: i64,
}

/// Haskell `GitFailureReceipt` — a failed git invocation, recorded verbatim so
/// the failure is diagnosable without re-running anything. Keeps stdout AND
/// stderr, never just the status.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "GitFailureReceipt")]
pub struct WtGitFailureReceipt {
    pub git_args: Vec<String>,
    pub git_cwd: String,
    /// `None` when the process was killed by a signal before exiting.
    pub git_exit_code: Option<i64>,
    pub git_stdout: String,
    pub git_stderr: String,
}

/// Haskell `WorktreeReceipt`. The id field is `tree_id`/`treeId` rather than
/// the PRD snippet's `worktreeId` — see `worktree_effect_def!`'s docs for why
/// that name had to yield to the function the PRD pins by signature.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "WorktreeReceipt")]
pub struct WtWorktreeReceipt {
    pub tree_id: WtWorktreeId,
    pub cwd: String,
    pub branch: WtBranchName,
    pub source_head: WtGitOid,
    pub snapshot_ref: Option<WtGitRef>,
    pub created_at: i64,
}

/// Haskell `WorktreeHandle` — a name plus its recorded facts, not an open
/// handle to anything.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "WorktreeHandle")]
pub struct WtWorktreeHandle {
    pub handle_receipt: WtWorktreeReceipt,
}

/// Haskell `WorktreeSummary`. `present` is a filesystem fact re-derived on each
/// listing rather than a recorded one, so a lost tree is listed rather than
/// failing the listing.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "WorktreeSummary")]
pub struct WtWorktreeSummary {
    pub summary_receipt: WtWorktreeReceipt,
    pub present: bool,
}

/// Haskell `EventId` — opaque RUNTIME identity, minted once per reconciliation
/// pass. A normal commit's `commit` and `headChanged` observations share one,
/// which is how a consumer tells two views of one change from two changes.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[core(name = "EventId")]
pub struct EvEventId {
    pub raw: i64,
}

/// Haskell `SubscriptionId` — one live `withHandler` registration.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[core(name = "SubscriptionId")]
pub struct EvSubscriptionId {
    pub raw: i64,
}

/// Haskell `Watch` — one (worktree, kind) pair a subscription observes. `<|>`
/// concatenates watches, so a merged `Event` is ONE subscription over several
/// watches rather than several subscriptions.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
pub enum EvWatch {
    WatchCommit(WtWorktreeId),
    WatchHead(WtWorktreeId),
}

/// Haskell `HeadChangeKind`. `UnknownChange` is a correct answer, not a
/// failure: inventing `Advanced` for what was actually a reset would send a
/// child rebasing onto a commit that no longer means what the claim said.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
pub enum EvHeadChangeKind {
    Advanced(Vec<WtGitOid>),
    Amended(WtGitOid, WtGitOid),
    Rewritten(Vec<(WtGitOid, WtGitOid)>),
    Rewound,
    Switched,
    UnknownChange,
}

/// Haskell `HeadChangeReceipt`.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "HeadChangeReceipt")]
pub struct EvHeadChangeReceipt {
    pub head_worktree: WtWorktreeId,
    /// `None` on the first observation of a worktree that had no recorded head.
    pub old_head: Option<WtGitOid>,
    pub new_head: WtGitOid,
    pub kind: EvHeadChangeKind,
    /// `None` on a detached HEAD.
    pub head_branch: Option<WtBranchName>,
    pub observed_at_ms: i64,
}

/// Haskell `CommitReceipt`.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
#[core(name = "CommitReceipt")]
pub struct EvCommitReceipt {
    pub commit_worktree: WtWorktreeId,
    pub oid: WtGitOid,
    pub parents: Vec<WtGitOid>,
    pub subject: String,
    pub author: String,
    pub committed_at_ms: i64,
    pub files: Vec<String>,
}

/// Haskell `RepositoryEvent` — one reconciled fact as it crosses the boundary.
/// The `EvEventId` rides on the wire rather than being minted per view,
/// because the SHARING is the information.
#[derive(ToCore, FromCore, Clone, Debug, PartialEq, Eq)]
pub enum EvRepositoryEvent {
    ObservedCommit(EvEventId, EvCommitReceipt),
    ObservedHeadChange(EvEventId, EvHeadChangeReceipt),
}

impl EvRepositoryEvent {
    /// The worktree this fact is about — what a subscription's watches match on.
    pub fn worktree(&self) -> &WtWorktreeId {
        match self {
            EvRepositoryEvent::ObservedCommit(_, r) => &r.commit_worktree,
            EvRepositoryEvent::ObservedHeadChange(_, r) => &r.head_worktree,
        }
    }

    /// Does `watch` select this fact? Kind AND worktree must both match: a
    /// `commit` subscription on tree A must not be woken by a head movement,
    /// nor by tree B's commit.
    pub fn matches(&self, watch: &EvWatch) -> bool {
        match (self, watch) {
            (EvRepositoryEvent::ObservedCommit(_, r), EvWatch::WatchCommit(w)) => {
                &r.commit_worktree == w
            }
            (EvRepositoryEvent::ObservedHeadChange(_, r), EvWatch::WatchHead(w)) => {
                &r.head_worktree == w
            }
            _ => false,
        }
    }
}

// ============================================================================
// Subagent wire types (PRD 18 lane 1 — coupled spawn), `Ag*`-prefixed.
//
// Same rules as the `Wt*`/`Ev*` families above: these do NOT derive
// `CoreRecord` (their Haskell decls are single-sourced from
// `subagent_effect_def!`'s `type_defs`), and field ORDER in each struct is the
// wire contract, matching those decls positionally. PROVISIONAL shapes — this
// lane exists to inform PRD 18's freezes, and renames land here + in the
// effect def together.
// ============================================================================

/// Haskell `AgentId` — Tidepool's identity for one agent, minted by the
/// runtime, never by a backend.
#[derive(ToCore, FromCore, Clone, Copy, Debug, PartialEq, Eq)]
#[core(name = "AgentId")]
pub struct AgAgentId {
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
}

/// Haskell `SpawnOutcome` — the verb's success payload. ToCore-only (carries
/// `AgCyclePayload`).
#[derive(ToCore, Clone, Debug, PartialEq)]
#[core(name = "SpawnOutcome")]
pub struct AgSpawnOutcome {
    pub outcome_run: AgWorkerRun,
    pub outcome_payload: AgCyclePayload,
    pub outcome_receipt: AgSpawnReceipt,
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
    out.push_str("{-# LANGUAGE NoImplicitPrelude, DuplicateRecordFields #-}\n\n");
    out.push_str(
        "-- | GENERATED from the Rust bridged-record structs in tidepool-bridge-effects\n",
    );
    out.push_str("-- (each carries `#[derive(CoreRecord)]`). DO NOT EDIT BY HAND: the Rust\n");
    out.push_str("-- struct is the single source of truth for field order / name / type, and\n");
    out.push_str("-- this file is regenerated + verified by the `bridged_records` test\n");
    out.push_str(
        "-- (`TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records`).\n",
    );
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
