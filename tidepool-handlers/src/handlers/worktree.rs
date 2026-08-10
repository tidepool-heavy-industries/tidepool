use std::path::PathBuf;

use tidepool_bridge_effects::{
    WtBranchName, WtDirtyPolicy, WtDirtySummary, WtGitFailureReceipt, WtGitOid, WtGitRef,
    WtInProgressKind, WtWorktreeHandle, WtWorktreeId, WtWorktreeReceipt, WtWorktreeSource,
    WtWorktreeSpec, WtWorktreeSummary,
};
use tidepool_worktree::create::{
    DirtyPolicy, WorktreeHandle, WorktreeManager, WorktreeSource, WorktreeSpec,
};
use tidepool_worktree::error::{
    DirtySummary, GitFailureReceipt, InProgressKind, WorktreeError as DomainWorktreeError,
};
use tidepool_worktree::git::GitCli;
use tidepool_worktree::id::{BranchName, GitOid, GitRef, WorktreeId};
#[cfg(test)]
use tidepool_worktree::registry::{WorktreeOrigin, WorktreeRecordStatus};
use tidepool_worktree::registry::{WorktreeReceipt, WorktreeRegistry, WorktreeSummary};

// ============================================================================
// Tag: Worktree (managed git worktrees — PRD 19, deliberately NOT in the
// default base_effects! row; see the boundary notes on this handler)
// ============================================================================

// WorktreeReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::worktree_effect_def!(crate::effect_glue::effect_rust_projection);

#[derive(Clone)]
pub struct WorktreeHandler {
    manager: WorktreeManager,
}

impl WorktreeHandler {
    /// `registry_root` and `worktree_root` must live OUTSIDE `source_repository`
    /// — see `tidepool-worktree/CLAUDE.md`'s "never dirty the source" rule.
    /// Fallible because opening the durable registry is (`WorktreeRegistry::open`).
    pub fn new(
        registry_root: PathBuf,
        worktree_root: PathBuf,
        source_repository: PathBuf,
    ) -> Result<Self, DomainWorktreeError> {
        let registry = WorktreeRegistry::open(&registry_root)?;
        Ok(Self {
            manager: WorktreeManager::new(
                GitCli::new(),
                registry,
                worktree_root,
                source_repository,
            ),
        })
    }
}

// ============================================================================
// Wire <-> domain conversions.
//
// `tidepool_bridge_effects::Wt*` are WIRE types, deliberately distinct from
// `tidepool_worktree`'s domain types of the same shape (the wire side carries
// `String` where the domain carries `PathBuf`/newtypes). Field ORDER in the
// wire structs is the wire contract (matches `worktree_effect_def!`'s
// `type_defs` positionally) and must never be reordered here.
//
// The handful marked `pub(crate)` are REUSED by `handlers::agent` — the
// Subagent wire types embed the Worktree ones (`AgSpawnWorkspace` carries a
// `WtWorktreeSpec`/`WtWorktreeId`, `AgWorkerRun` a `WtWorktreeHandle`, and the
// wire `SpawnError` carries this module's wire `WorktreeError`), so a second
// copy over there would be two conversions to keep in step with one contract.
// ============================================================================

pub(crate) fn worktree_id_to_wire(id: &WorktreeId) -> WtWorktreeId {
    WtWorktreeId {
        raw: id.as_str().to_string(),
    }
}

/// The trust boundary where a wire id becomes a domain id. A raw value that
/// is not path-safe (separators, dot-dots, empty, over-long) is rejected as
/// `WorktreeNotRegistered` — semantically true (no such id was ever minted)
/// and, load-bearingly, BEFORE the value can reach the registry/binding code
/// that joins ids into file paths.
pub(crate) fn worktree_id_from_wire(id: &WtWorktreeId) -> Result<WorktreeId, WorktreeError> {
    if !WorktreeId::is_path_safe(&id.raw) {
        return Err(never_registered(id));
    }
    Ok(WorktreeId::from_raw(id.raw.clone()))
}

fn git_oid_to_wire(oid: &GitOid) -> WtGitOid {
    WtGitOid {
        raw: oid.as_str().to_string(),
    }
}

fn git_ref_to_wire(r: &GitRef) -> WtGitRef {
    WtGitRef {
        raw: r.as_str().to_string(),
    }
}

fn git_ref_from_wire(r: &WtGitRef) -> GitRef {
    GitRef::from_raw(r.raw.clone())
}

fn branch_name_to_wire(b: &BranchName) -> WtBranchName {
    WtBranchName {
        raw: b.as_str().to_string(),
    }
}

fn dirty_summary_to_wire(d: &DirtySummary) -> WtDirtySummary {
    WtDirtySummary {
        staged: d.staged.clone(),
        unstaged: d.unstaged.clone(),
        untracked: d.untracked.clone(),
        ignored_excluded: d.ignored_excluded as i64,
    }
}

fn in_progress_kind_to_wire(k: InProgressKind) -> WtInProgressKind {
    match k {
        InProgressKind::Merge => WtInProgressKind::InProgressMerge,
        InProgressKind::Rebase => WtInProgressKind::InProgressRebase,
        InProgressKind::CherryPick => WtInProgressKind::InProgressCherryPick,
        InProgressKind::Revert => WtInProgressKind::InProgressRevert,
        InProgressKind::Bisect => WtInProgressKind::InProgressBisect,
    }
}

fn git_failure_receipt_to_wire(r: GitFailureReceipt) -> WtGitFailureReceipt {
    WtGitFailureReceipt {
        git_args: r.args,
        git_cwd: r.cwd.to_string_lossy().into_owned(),
        git_exit_code: r.exit_code.map(i64::from),
        git_stdout: r.stdout,
        git_stderr: r.stderr,
    }
}

fn worktree_source_from_wire(source: WtWorktreeSource) -> Result<WorktreeSource, WorktreeError> {
    Ok(match source {
        WtWorktreeSource::SourceCurrentRepository => WorktreeSource::CurrentRepository,
        WtWorktreeSource::SourceRef(r) => WorktreeSource::Ref(git_ref_from_wire(&r)),
        WtWorktreeSource::SourceWorktree(id) => {
            WorktreeSource::Worktree(worktree_id_from_wire(&id)?)
        }
    })
}

fn dirty_policy_from_wire(policy: WtDirtyPolicy) -> DirtyPolicy {
    match policy {
        WtDirtyPolicy::RequireClean => DirtyPolicy::RequireClean,
        WtDirtyPolicy::AllowDirtySnapshot => DirtyPolicy::AllowDirtySnapshot,
    }
}

pub(crate) fn spec_from_wire(spec: WtWorktreeSpec) -> Result<WorktreeSpec, WorktreeError> {
    Ok(WorktreeSpec {
        source: worktree_source_from_wire(spec.spec_source)?,
        label: spec.spec_label,
        dirty_policy: dirty_policy_from_wire(spec.spec_dirty_policy),
    })
}

fn receipt_to_wire(r: &WorktreeReceipt) -> WtWorktreeReceipt {
    WtWorktreeReceipt {
        tree_id: worktree_id_to_wire(&r.worktree_id),
        cwd: r.cwd.to_string_lossy().into_owned(),
        branch: branch_name_to_wire(&r.branch),
        source_head: git_oid_to_wire(&r.source_head),
        snapshot_ref: r.snapshot_ref.as_ref().map(git_ref_to_wire),
        created_at: r.created_at_ms,
    }
}

pub(crate) fn handle_to_wire(h: &WorktreeHandle) -> WtWorktreeHandle {
    WtWorktreeHandle {
        handle_receipt: receipt_to_wire(h.receipt()),
    }
}

fn summary_to_wire(s: &WorktreeSummary) -> WtWorktreeSummary {
    WtWorktreeSummary {
        summary_receipt: receipt_to_wire(&s.receipt),
        present: s.present,
    }
}

/// Total map from the domain `WorktreeError` (`tidepool-worktree/src/error.rs`)
/// to the wire `WorktreeError` (generated by `worktree_effect_def!`'s `errors`
/// block, now ten variants in the same order — the original seven plus
/// `WorktreeNotRegistered`/`InvalidRegistryRoot`/`StorageFailure`, added to the
/// wire block once the storage-error lane grew the domain type to match).
pub(crate) fn error_to_wire(e: DomainWorktreeError) -> WorktreeError {
    match e {
        DomainWorktreeError::SourceDirty(d) => {
            WorktreeError::SourceDirty(dirty_summary_to_wire(&d))
        }
        DomainWorktreeError::NotARepository(p) => {
            WorktreeError::NotARepository(p.to_string_lossy().into_owned())
        }
        DomainWorktreeError::WorktreeLost(id) => {
            WorktreeError::WorktreeLost(worktree_id_to_wire(&id))
        }
        // Distinct from `WorktreeLost` on the wire, matching the domain's own
        // reasoning (tidepool-worktree/src/error.rs): collapsing "never
        // registered" into a neighbour hides a typo behind a data-loss
        // report. See `never_registered` below, which builds this same
        // variant for the `lookup`/`worktree_head_of`/`worktree_branch_of`
        // `Ok(None)` case.
        DomainWorktreeError::WorktreeNotRegistered(id) => {
            WorktreeError::WorktreeNotRegistered(worktree_id_to_wire(&id))
        }
        DomainWorktreeError::InvalidRegistryRoot { root, inside } => {
            WorktreeError::InvalidRegistryRoot(
                root.to_string_lossy().into_owned(),
                inside.to_string_lossy().into_owned(),
            )
        }
        DomainWorktreeError::StorageFailure { path, detail } => {
            WorktreeError::StorageFailure(path.to_string_lossy().into_owned(), detail)
        }
        DomainWorktreeError::DirtySubmoduleUnsupported(p) => {
            WorktreeError::DirtySubmoduleUnsupported(p.to_string_lossy().into_owned())
        }
        DomainWorktreeError::SourceOperationInProgress(k) => {
            WorktreeError::SourceOperationInProgress(in_progress_kind_to_wire(k))
        }
        DomainWorktreeError::WorktreeBusy { worktree, holder } => {
            WorktreeError::WorktreeBusy(worktree_id_to_wire(&worktree), holder)
        }
        DomainWorktreeError::GitFailure(r) => {
            WorktreeError::GitFailure(git_failure_receipt_to_wire(r))
        }
    }
}

/// Build a wire `WorktreeNotRegistered` for a `WorktreeId` that was never
/// registered — distinct from `WorktreeLost` (registered, then gone from
/// disk). See `worktree_lookup`'s doc comment and `error_to_wire`'s
/// `WorktreeNotRegistered` arm above, which this stays consistent with.
pub(crate) fn never_registered(tree_id: &WtWorktreeId) -> WorktreeError {
    WorktreeError::WorktreeNotRegistered(tree_id.clone())
}

impl WorktreeHandler {
    // Errors-tagged verbs: total in `WorktreeError`, no `cx` — the dispatch
    // arm wraps the `Result` via `cx.respond` (Ok→Right, Err→Left). See #335.

    fn worktree_create(&mut self, spec: WtWorktreeSpec) -> Result<WtWorktreeHandle, WorktreeError> {
        let domain_spec = spec_from_wire(spec)?;
        let handle = self.manager.create(&domain_spec).map_err(error_to_wire)?;
        Ok(handle_to_wire(&handle))
    }

    /// `Ok(None)` from `WorktreeManager::lookup` (the id was NEVER
    /// registered — a typo-shaped miss) must stay distinguishable from
    /// `Err(WorktreeLost)` (registered, then gone from disk — data loss); see
    /// `tidepool-worktree/src/registry.rs`'s `WorktreeRegistry::get` doc and
    /// PRD 19. `Ok(None)` is spelled here as the wire `WorktreeNotRegistered`
    /// variant (`never_registered` above), which the `errors` block carries
    /// specifically to preserve this distinction — never collapsed onto
    /// `WorktreeLost`'s tag, which would erase exactly what PRD 19 asks be
    /// kept visible.
    fn worktree_lookup(
        &mut self,
        tree_id: WtWorktreeId,
    ) -> Result<WtWorktreeHandle, WorktreeError> {
        let id = worktree_id_from_wire(&tree_id)?;
        match self.manager.lookup(&id).map_err(error_to_wire)? {
            Some(handle) => Ok(handle_to_wire(&handle)),
            None => Err(never_registered(&tree_id)),
        }
    }

    fn worktree_list(&mut self) -> Result<Vec<WtWorktreeSummary>, WorktreeError> {
        let summaries = self.manager.list().map_err(error_to_wire)?;
        Ok(summaries.iter().map(summary_to_wire).collect())
    }

    /// Reads the branch fresh from git rather than returning the receipt's
    /// recorded one — reconciled inspection is the only source of truth (see
    /// `tidepool-worktree/CLAUDE.md`).
    fn worktree_branch_of(&mut self, tree_id: WtWorktreeId) -> Result<WtBranchName, WorktreeError> {
        let id = worktree_id_from_wire(&tree_id)?;
        let handle = self
            .manager
            .lookup(&id)
            .map_err(error_to_wire)?
            .ok_or_else(|| never_registered(&tree_id))?;
        let out = self
            .manager
            .git()
            .try_run(handle.cwd(), &["rev-parse", "--abbrev-ref", "HEAD"])
            .map_err(error_to_wire)?;
        Ok(branch_name_to_wire(&BranchName::from_raw(
            out.trimmed().to_string(),
        )))
    }

    /// A FRESH git read of this worktree's current HEAD. Specifically NOT the
    /// handle's recorded `source_head` (the seed the managed branch was
    /// rooted at — the stale answer) and NOT the event monitor's
    /// last-observed baseline. A resident spanning cycles compares this
    /// against its own checkpointed head to close the window where HEAD
    /// moved after one monitor cycle unregistered and before the next
    /// registered; returning either recorded value would leave that window
    /// silently open. Reads through the same substrate as
    /// `worktree_branch_of` — never spawns git itself, never caches.
    ///
    /// Added to `worktree_effect_def!` on a sibling branch (`wt-surface`)
    /// after this worktree forked, so the generated dispatch here has no arm
    /// calling it yet — it becomes live at fold. Until then it has no caller
    /// in this tree.
    ///
    /// A thin delegation to `WorktreeManager::worktree_head`, a fresh
    /// `rev-parse HEAD` on every call. That method takes a `&WorktreeHandle`,
    /// not a `&WorktreeId`, so — same as `worktree_branch_of` — the id is
    /// resolved to a handle first; a lookup failure (never-registered or
    /// lost) surfaces as the typed error rather than being swallowed. No
    /// local `rev-parse`, no `source_head` shortcut.
    #[allow(dead_code)]
    fn worktree_head_of(&mut self, tree_id: WtWorktreeId) -> Result<WtGitOid, WorktreeError> {
        let id = worktree_id_from_wire(&tree_id)?;
        let handle = self
            .manager
            .lookup(&id)
            .map_err(error_to_wire)?
            .ok_or_else(|| never_registered(&tree_id))?;
        let head = self.manager.worktree_head(&handle).map_err(error_to_wire)?;
        Ok(git_oid_to_wire(&head))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_dirty_summary() -> DirtySummary {
        DirtySummary {
            staged: vec!["a.txt".to_string()],
            unstaged: vec!["b.txt".to_string()],
            untracked: vec!["c.txt".to_string()],
            ignored_excluded: 3,
        }
    }

    fn sample_git_failure_receipt() -> GitFailureReceipt {
        GitFailureReceipt {
            args: vec!["status".to_string()],
            cwd: PathBuf::from("/repo"),
            exit_code: Some(128),
            stdout: String::new(),
            stderr: "fatal: not a git repository".to_string(),
        }
    }

    // -----------------------------------------------------------------
    // Spec conversion: every WorktreeSource variant x both DirtyPolicy
    // values. Pure, testable today without touching any todo!().
    // -----------------------------------------------------------------

    #[test]
    fn spec_from_wire_round_trips_current_repository_require_clean() {
        let wire = WtWorktreeSpec {
            spec_source: WtWorktreeSource::SourceCurrentRepository,
            spec_label: "dev-tree/root".to_string(),
            spec_dirty_policy: WtDirtyPolicy::RequireClean,
        };
        let domain = spec_from_wire(wire).expect("valid wire spec");
        assert_eq!(
            domain,
            WorktreeSpec {
                source: WorktreeSource::CurrentRepository,
                label: "dev-tree/root".to_string(),
                dirty_policy: DirtyPolicy::RequireClean,
            }
        );
    }

    #[test]
    fn spec_from_wire_round_trips_ref_allow_dirty_snapshot() {
        let wire = WtWorktreeSpec {
            spec_source: WtWorktreeSource::SourceRef(WtGitRef {
                raw: "refs/heads/main".to_string(),
            }),
            spec_label: "reviewer".to_string(),
            spec_dirty_policy: WtDirtyPolicy::AllowDirtySnapshot,
        };
        let domain = spec_from_wire(wire).expect("valid wire spec");
        assert_eq!(
            domain,
            WorktreeSpec {
                source: WorktreeSource::Ref(GitRef::from_raw("refs/heads/main")),
                label: "reviewer".to_string(),
                dirty_policy: DirtyPolicy::AllowDirtySnapshot,
            }
        );
    }

    #[test]
    fn spec_from_wire_round_trips_worktree_source() {
        let wire = WtWorktreeSpec {
            spec_source: WtWorktreeSource::SourceWorktree(WtWorktreeId {
                raw: "wt-abc123".to_string(),
            }),
            spec_label: "child-of-abc123".to_string(),
            spec_dirty_policy: WtDirtyPolicy::RequireClean,
        };
        let domain = spec_from_wire(wire).expect("valid wire spec");
        assert_eq!(
            domain,
            WorktreeSpec {
                source: WorktreeSource::Worktree(WorktreeId::from_raw("wt-abc123")),
                label: "child-of-abc123".to_string(),
                dirty_policy: DirtyPolicy::RequireClean,
            }
        );
    }

    // -----------------------------------------------------------------
    // Receipt/handle/summary conversion: snapshot_ref present and absent.
    // -----------------------------------------------------------------

    fn sample_receipt(snapshot_ref: Option<GitRef>) -> WorktreeReceipt {
        WorktreeReceipt {
            worktree_id: WorktreeId::from_raw("wt-1"),
            cwd: PathBuf::from("/worktrees/wt-1"),
            branch: BranchName::from_raw("tidepool/worktree/wt-1"),
            source_head: GitOid::from_raw("deadbeef"),
            snapshot_ref,
            origin: WorktreeOrigin::CurrentRepository,
            source_repository: PathBuf::from("/repo"),
            created_at_ms: 1_700_000_000_000,
            status: WorktreeRecordStatus::Finalized,
        }
    }

    #[test]
    fn receipt_to_wire_carries_snapshot_ref_when_present() {
        let receipt = sample_receipt(Some(GitRef::from_raw("refs/tidepool/snapshots/wt-1")));
        let wire = receipt_to_wire(&receipt);
        assert_eq!(
            wire.snapshot_ref,
            Some(WtGitRef {
                raw: "refs/tidepool/snapshots/wt-1".to_string()
            })
        );
        assert_eq!(
            wire.tree_id,
            WtWorktreeId {
                raw: "wt-1".to_string()
            }
        );
        assert_eq!(wire.cwd, "/worktrees/wt-1");
        assert_eq!(
            wire.branch,
            WtBranchName {
                raw: "tidepool/worktree/wt-1".to_string()
            }
        );
        assert_eq!(
            wire.source_head,
            WtGitOid {
                raw: "deadbeef".to_string()
            }
        );
        assert_eq!(wire.created_at, 1_700_000_000_000);
    }

    #[test]
    fn receipt_to_wire_has_no_snapshot_ref_when_absent() {
        let receipt = sample_receipt(None);
        let wire = receipt_to_wire(&receipt);
        assert_eq!(wire.snapshot_ref, None);
    }

    #[test]
    fn handle_to_wire_wraps_the_receipt() {
        let receipt = sample_receipt(None);
        let handle = WorktreeHandle::from_receipt(receipt.clone());
        let wire = handle_to_wire(&handle);
        assert_eq!(wire.handle_receipt, receipt_to_wire(&receipt));
    }

    #[test]
    fn summary_to_wire_carries_presence() {
        let receipt = sample_receipt(None);
        let present = WorktreeSummary {
            receipt: receipt.clone(),
            present: false,
        };
        let wire = summary_to_wire(&present);
        assert_eq!(wire.summary_receipt, receipt_to_wire(&receipt));
        assert!(!wire.present);
    }

    // -----------------------------------------------------------------
    // WorktreeError: one case per domain variant, asserting the wire
    // variant and its payload.
    // -----------------------------------------------------------------

    #[test]
    fn error_to_wire_source_dirty() {
        let wire = error_to_wire(DomainWorktreeError::SourceDirty(sample_dirty_summary()));
        assert_eq!(
            wire,
            WorktreeError::SourceDirty(WtDirtySummary {
                staged: vec!["a.txt".to_string()],
                unstaged: vec!["b.txt".to_string()],
                untracked: vec!["c.txt".to_string()],
                ignored_excluded: 3,
            })
        );
    }

    #[test]
    fn error_to_wire_not_a_repository() {
        let wire = error_to_wire(DomainWorktreeError::NotARepository(PathBuf::from(
            "/not/a/repo",
        )));
        assert_eq!(
            wire,
            WorktreeError::NotARepository("/not/a/repo".to_string())
        );
    }

    #[test]
    fn error_to_wire_worktree_lost() {
        let wire = error_to_wire(DomainWorktreeError::WorktreeLost(WorktreeId::from_raw(
            "wt-9",
        )));
        assert_eq!(
            wire,
            WorktreeError::WorktreeLost(WtWorktreeId {
                raw: "wt-9".to_string()
            })
        );
    }

    #[test]
    fn error_to_wire_dirty_submodule_unsupported() {
        let wire = error_to_wire(DomainWorktreeError::DirtySubmoduleUnsupported(
            PathBuf::from("vendor/sub"),
        ));
        assert_eq!(
            wire,
            WorktreeError::DirtySubmoduleUnsupported("vendor/sub".to_string())
        );
    }

    #[test]
    fn error_to_wire_source_operation_in_progress() {
        let wire = error_to_wire(DomainWorktreeError::SourceOperationInProgress(
            InProgressKind::Rebase,
        ));
        assert_eq!(
            wire,
            WorktreeError::SourceOperationInProgress(WtInProgressKind::InProgressRebase)
        );
    }

    #[test]
    fn error_to_wire_worktree_busy() {
        let wire = error_to_wire(DomainWorktreeError::WorktreeBusy {
            worktree: WorktreeId::from_raw("wt-2"),
            holder: "agent-7".to_string(),
        });
        assert_eq!(
            wire,
            WorktreeError::WorktreeBusy(
                WtWorktreeId {
                    raw: "wt-2".to_string()
                },
                "agent-7".to_string()
            )
        );
    }

    #[test]
    fn error_to_wire_git_failure() {
        let wire = error_to_wire(DomainWorktreeError::GitFailure(sample_git_failure_receipt()));
        assert_eq!(
            wire,
            WorktreeError::GitFailure(WtGitFailureReceipt {
                git_args: vec!["status".to_string()],
                git_cwd: "/repo".to_string(),
                git_exit_code: Some(128),
                git_stdout: String::new(),
                git_stderr: "fatal: not a git repository".to_string(),
            })
        );
    }

    #[test]
    fn error_to_wire_worktree_not_registered() {
        let wire = error_to_wire(DomainWorktreeError::WorktreeNotRegistered(
            WorktreeId::from_raw("wt-typo"),
        ));
        assert_eq!(
            wire,
            WorktreeError::WorktreeNotRegistered(WtWorktreeId {
                raw: "wt-typo".to_string()
            })
        );
    }

    #[test]
    fn error_to_wire_invalid_registry_root() {
        let wire = error_to_wire(DomainWorktreeError::InvalidRegistryRoot {
            root: PathBuf::from("/repo/.tidepool-registry"),
            inside: PathBuf::from("/repo"),
        });
        assert_eq!(
            wire,
            WorktreeError::InvalidRegistryRoot(
                "/repo/.tidepool-registry".to_string(),
                "/repo".to_string()
            )
        );
    }

    #[test]
    fn error_to_wire_storage_failure() {
        let wire = error_to_wire(DomainWorktreeError::StorageFailure {
            path: PathBuf::from("/registry/wt-1.json"),
            detail: "No space left on device".to_string(),
        });
        assert_eq!(
            wire,
            WorktreeError::StorageFailure(
                "/registry/wt-1.json".to_string(),
                "No space left on device".to_string()
            )
        );
    }

    #[test]
    fn never_registered_is_distinct_from_worktree_lost() {
        let id = WtWorktreeId {
            raw: "wt-ghost".to_string(),
        };
        let unregistered = never_registered(&id);
        let lost = error_to_wire(DomainWorktreeError::WorktreeLost(WorktreeId::from_raw(
            "wt-ghost",
        )));
        assert_ne!(unregistered, lost);
        assert!(matches!(
            unregistered,
            WorktreeError::WorktreeNotRegistered(_)
        ));
        assert!(matches!(lost, WorktreeError::WorktreeLost(_)));
    }

    #[test]
    fn never_registered_spells_worktree_not_registered_with_the_given_id() {
        let id = WtWorktreeId {
            raw: "wt-typo".to_string(),
        };
        assert_eq!(
            never_registered(&id),
            WorktreeError::WorktreeNotRegistered(WtWorktreeId {
                raw: "wt-typo".to_string()
            })
        );
    }

    /// The wire boundary rejects an id that could act as a path, BEFORE it
    /// becomes a domain `WorktreeId` (which registry/binding code joins into
    /// file paths). Rejection spells `WorktreeNotRegistered` — no id outside
    /// the minted alphabet was ever registered, and the caller learns nothing
    /// about the filesystem.
    #[test]
    fn traversal_shaped_wire_ids_are_rejected_at_the_boundary() {
        for evil in [
            "../../../etc/passwd",
            "a/b",
            "a\\b",
            "..",
            ".",
            "",
            "wt-abc/../../x",
        ] {
            let wire = WtWorktreeId {
                raw: evil.to_string(),
            };
            assert_eq!(
                worktree_id_from_wire(&wire),
                Err(never_registered(&wire)),
                "{evil:?} must be rejected before becoming a domain id"
            );
        }
        let minted = WtWorktreeId {
            raw: "wt-19c8-2a4d-0-deadbeef".to_string(),
        };
        assert_eq!(
            worktree_id_from_wire(&minted),
            Ok(WorktreeId::from_raw("wt-19c8-2a4d-0-deadbeef"))
        );
    }
}
