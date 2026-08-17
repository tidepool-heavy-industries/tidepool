use std::path::PathBuf;

use tidepool_bridge_effects::{
    WireError, WtBranchName, WtDirtySummary, WtGitFailureReceipt, WtGitOid, WtWorktreeHandle,
    WtWorktreeId, WtWorktreeReceipt, WtWorktreeSource, WtWorktreeSpec, WtWorktreeSummary,
};
use tidepool_worktree::create::{WorktreeHandle, WorktreeManager, WorktreeSource, WorktreeSpec};
use tidepool_worktree::error::{
    DirtySummary, GitFailureReceipt, WorktreeError as DomainWorktreeError,
};
use tidepool_worktree::git::GitCli;
use tidepool_worktree::id::{BranchName, WorktreeId};
#[cfg(test)]
use tidepool_worktree::registry::{WorktreeOrigin, WorktreeRecordStatus};
use tidepool_worktree::registry::{WorktreeReceipt, WorktreeRegistry, WorktreeSummary};

// ============================================================================
// Tag: Worktree (managed git worktrees — PRD 19, deliberately NOT in the
// default base_effects! row)
// ============================================================================

// WorktreeReq, WorktreeError, DescribeEffect and the EffectHandler dispatch are
// GENERATED from the `tidepool-protocol` schema (PRD 22 phase 3) — re-exported
// here so the public paths (`tidepool_handlers::WorktreeReq`,
// `tidepool_handlers::WorktreeError`) are unchanged. Only the handler struct and
// the per-verb method bodies below are hand-written.
pub use crate::generated::worktree::{WorktreeError, WorktreeReq};

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
// `String` where the domain carries `PathBuf`/newtypes). Both the wire structs
// and the Haskell decls they cross to are GENERATED from one ordered field list
// in `tidepool-protocol`, so their field order cannot disagree — there is no
// longer a positional invariant to keep by hand.
//
// The MECHANICAL conversions (four identity newtypes, `DirtyPolicy`,
// `InProgressKind`) come from `crate::generated::worktree_adapters`; the ones
// carrying a DECISION stay hand-written here, and the schema records which is
// which (`AdapterKind::HandWritten(reason)`) so a later lane can see what it
// may safely regenerate without re-deriving the judgement.
//
// The handful re-exported `pub(crate)` are REUSED by `handlers::agent` — the
// Subagent wire types embed the Worktree ones (`AgSpawnWorkspace` carries a
// `WtWorktreeSpec`/`WtWorktreeId`, `AgWorkerRun` a `WtWorktreeHandle`, and the
// wire `SpawnError` carries this module's wire `WorktreeError`), so a second
// copy over there would be two conversions to keep in step with one contract.
// ============================================================================

pub(crate) use crate::generated::worktree_adapters::{
    branch_name_to_wire, dirty_policy_from_wire, git_oid_to_wire, git_ref_from_wire,
    git_ref_to_wire, in_progress_kind_to_wire, worktree_id_to_wire,
};

/// The trust boundary where a wire id becomes a domain id.
///
/// The MECHANICAL half is generated: `WtWorktreeId::new` is the schema's
/// `Validation::Segment { max_len: 128, extra_allowed: "-_" }`, which is
/// byte-for-byte `tidepool_worktree::WorktreeId::is_path_safe` expressed as
/// declared data (`worktree_wire_segment_policy_agrees_with_is_path_safe`
/// pins that the two agree).
///
/// The SEMANTIC half stays here, and is the reason this conversion is not
/// generated: a rejected id is spelled `WorktreeNotRegistered` — true, because
/// no id outside the minted alphabet was ever registered, and it tells the
/// caller nothing about the filesystem. The rejection happens BEFORE the value
/// can reach the registry/binding code that joins ids into file paths.
pub(crate) fn worktree_id_from_wire(id: &WtWorktreeId) -> Result<WorktreeId, WorktreeError> {
    match WtWorktreeId::new(id.raw.clone()) {
        Ok(checked) => Ok(WorktreeId::from_raw(checked.raw)),
        Err(WireError::Empty { .. } | WireError::InvalidSegment { .. }) => {
            Err(never_registered(id))
        }
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
/// to the wire `WorktreeError` (generated from the schema's `errors` block) — ten variants on both sides. No wildcard arm below, so a domain
/// variant added without a wire counterpart fails this match's exhaustiveness
/// check at compile time.
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

    pub(crate) fn worktree_create(
        &mut self,
        spec: WtWorktreeSpec,
    ) -> Result<WtWorktreeHandle, WorktreeError> {
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
    pub(crate) fn worktree_lookup(
        &mut self,
        tree_id: WtWorktreeId,
    ) -> Result<WtWorktreeHandle, WorktreeError> {
        let id = worktree_id_from_wire(&tree_id)?;
        match self.manager.lookup(&id).map_err(error_to_wire)? {
            Some(handle) => Ok(handle_to_wire(&handle)),
            None => Err(never_registered(&tree_id)),
        }
    }

    pub(crate) fn worktree_list(&mut self) -> Result<Vec<WtWorktreeSummary>, WorktreeError> {
        let summaries = self.manager.list().map_err(error_to_wire)?;
        Ok(summaries.iter().map(summary_to_wire).collect())
    }

    /// Reads the branch fresh from git rather than returning the receipt's
    /// recorded one — reconciled inspection is the only source of truth (see
    /// `tidepool-worktree/CLAUDE.md`).
    pub(crate) fn worktree_branch_of(
        &mut self,
        tree_id: WtWorktreeId,
    ) -> Result<WtBranchName, WorktreeError> {
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
    /// A thin delegation to `WorktreeManager::worktree_head`, a fresh
    /// `rev-parse HEAD` on every call. That method takes a `&WorktreeHandle`,
    /// not a `&WorktreeId`, so — same as `worktree_branch_of` — the id is
    /// resolved to a handle first; a lookup failure (never-registered or
    /// lost) surfaces as the typed error rather than being swallowed. No
    /// local `rev-parse`, no `source_head` shortcut.
    #[allow(dead_code)]
    pub(crate) fn worktree_head_of(
        &mut self,
        tree_id: WtWorktreeId,
    ) -> Result<WtGitOid, WorktreeError> {
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
    // The wire enums/newtypes these tables construct directly. The module body
    // no longer names them — its mechanical conversions are generated — but the
    // tables below still build wire values by hand, which is the point: they
    // assert against literals, not against the conversions under test.
    use tidepool_bridge_effects::{WtDirtyPolicy, WtGitRef, WtInProgressKind};
    use tidepool_worktree::create::DirtyPolicy;
    use tidepool_worktree::error::InProgressKind;
    use tidepool_worktree::id::{GitOid, GitRef};

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

    /// Every `WorktreeSource` variant crossed with a `DirtyPolicy` value —
    /// pure, testable today without touching any `todo!()`.
    #[test]
    fn spec_from_wire_round_trips() {
        let cases: Vec<(&str, WtWorktreeSpec, WorktreeSpec)> = vec![
            (
                "current_repository_require_clean",
                WtWorktreeSpec {
                    spec_source: WtWorktreeSource::SourceCurrentRepository,
                    spec_label: "dev-tree/root".to_string(),
                    spec_dirty_policy: WtDirtyPolicy::RequireClean,
                },
                WorktreeSpec {
                    source: WorktreeSource::CurrentRepository,
                    label: "dev-tree/root".to_string(),
                    dirty_policy: DirtyPolicy::RequireClean,
                },
            ),
            (
                "ref_allow_dirty_snapshot",
                WtWorktreeSpec {
                    spec_source: WtWorktreeSource::SourceRef(WtGitRef {
                        raw: "refs/heads/main".to_string(),
                    }),
                    spec_label: "reviewer".to_string(),
                    spec_dirty_policy: WtDirtyPolicy::AllowDirtySnapshot,
                },
                WorktreeSpec {
                    source: WorktreeSource::Ref(GitRef::from_raw("refs/heads/main")),
                    label: "reviewer".to_string(),
                    dirty_policy: DirtyPolicy::AllowDirtySnapshot,
                },
            ),
            (
                "worktree_source",
                WtWorktreeSpec {
                    spec_source: WtWorktreeSource::SourceWorktree(WtWorktreeId {
                        raw: "wt-abc123".to_string(),
                    }),
                    spec_label: "child-of-abc123".to_string(),
                    spec_dirty_policy: WtDirtyPolicy::RequireClean,
                },
                WorktreeSpec {
                    source: WorktreeSource::Worktree(WorktreeId::from_raw("wt-abc123")),
                    label: "child-of-abc123".to_string(),
                    dirty_policy: DirtyPolicy::RequireClean,
                },
            ),
        ];

        for (label, wire, expected) in cases {
            let domain = spec_from_wire(wire).expect("valid wire spec");
            assert_eq!(domain, expected, "case: {label}");
        }
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

    /// One row per `DomainWorktreeError` variant. `error_to_wire`'s own
    /// match (worktree.rs) has no wildcard arm, so a new domain variant
    /// fails THAT compile first — same exhaustiveness discipline as
    /// agent.rs's `handler_stage_to_wire_covers_every_stage`; this table
    /// just needs to stay in sync with it.
    #[test]
    fn error_to_wire_covers_every_variant() {
        let cases: Vec<(&str, DomainWorktreeError, WorktreeError)> = vec![
            (
                "source_dirty",
                DomainWorktreeError::SourceDirty(sample_dirty_summary()),
                WorktreeError::SourceDirty(WtDirtySummary {
                    staged: vec!["a.txt".to_string()],
                    unstaged: vec!["b.txt".to_string()],
                    untracked: vec!["c.txt".to_string()],
                    ignored_excluded: 3,
                }),
            ),
            (
                "not_a_repository",
                DomainWorktreeError::NotARepository(PathBuf::from("/not/a/repo")),
                WorktreeError::NotARepository("/not/a/repo".to_string()),
            ),
            (
                "worktree_lost",
                DomainWorktreeError::WorktreeLost(WorktreeId::from_raw("wt-9")),
                WorktreeError::WorktreeLost(WtWorktreeId {
                    raw: "wt-9".to_string(),
                }),
            ),
            (
                "dirty_submodule_unsupported",
                DomainWorktreeError::DirtySubmoduleUnsupported(PathBuf::from("vendor/sub")),
                WorktreeError::DirtySubmoduleUnsupported("vendor/sub".to_string()),
            ),
            (
                "source_operation_in_progress",
                DomainWorktreeError::SourceOperationInProgress(InProgressKind::Rebase),
                WorktreeError::SourceOperationInProgress(WtInProgressKind::InProgressRebase),
            ),
            (
                "worktree_busy",
                DomainWorktreeError::WorktreeBusy {
                    worktree: WorktreeId::from_raw("wt-2"),
                    holder: "agent-7".to_string(),
                },
                WorktreeError::WorktreeBusy(
                    WtWorktreeId {
                        raw: "wt-2".to_string(),
                    },
                    "agent-7".to_string(),
                ),
            ),
            (
                "git_failure",
                DomainWorktreeError::GitFailure(sample_git_failure_receipt()),
                WorktreeError::GitFailure(WtGitFailureReceipt {
                    git_args: vec!["status".to_string()],
                    git_cwd: "/repo".to_string(),
                    git_exit_code: Some(128),
                    git_stdout: String::new(),
                    git_stderr: "fatal: not a git repository".to_string(),
                }),
            ),
            (
                "worktree_not_registered",
                DomainWorktreeError::WorktreeNotRegistered(WorktreeId::from_raw("wt-typo")),
                WorktreeError::WorktreeNotRegistered(WtWorktreeId {
                    raw: "wt-typo".to_string(),
                }),
            ),
            (
                "invalid_registry_root",
                DomainWorktreeError::InvalidRegistryRoot {
                    root: PathBuf::from("/repo/.tidepool-registry"),
                    inside: PathBuf::from("/repo"),
                },
                WorktreeError::InvalidRegistryRoot(
                    "/repo/.tidepool-registry".to_string(),
                    "/repo".to_string(),
                ),
            ),
            (
                "storage_failure",
                DomainWorktreeError::StorageFailure {
                    path: PathBuf::from("/registry/wt-1.json"),
                    detail: "No space left on device".to_string(),
                },
                WorktreeError::StorageFailure(
                    "/registry/wt-1.json".to_string(),
                    "No space left on device".to_string(),
                ),
            ),
        ];

        for (label, domain, expected) in cases {
            assert_eq!(error_to_wire(domain), expected, "case: {label}");
        }
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

    /// The duplication §11.4 names rather than hides, guarded.
    ///
    /// The path-safety policy is expressed TWICE: as schema data generated into
    /// `WtWorktreeId::new` (`Validation::Segment { max_len: 128, extra_allowed:
    /// "-_" }`), and as `tidepool_worktree::WorktreeId::is_path_safe`. They
    /// cannot be unified — `tidepool-worktree` is a DOMAIN crate and must not
    /// depend on the bridge layer, and the bridge layer is lower than the
    /// domain — so the crate direction forces the copy and this test is the
    /// mitigation.
    ///
    /// Byte-oriented on both sides, deliberately: an id is joined into a
    /// filesystem path as ONE component, so a char-oriented check would accept
    /// multi-byte input the domain rejects. The corpus includes every
    /// traversal-shaped input `traversal_shaped_wire_ids_are_rejected_at_the_
    /// boundary` pins, plus the multi-byte and boundary-length cases that would
    /// catch the two policies drifting apart in a way the traversal set cannot.
    #[test]
    fn worktree_wire_segment_policy_agrees_with_is_path_safe() {
        let corpus: Vec<String> = [
            // the traversal-shaped set the boundary test pins
            "../../../etc/passwd",
            "a/b",
            "a\\b",
            "..",
            ".",
            "",
            "wt-abc/../../x",
            // minted shapes
            "wt-19c8-2a4d-0-deadbeef",
            "wt_1",
            "A",
            "0",
            // alphabet edges: allowed extras vs everything else
            "-",
            "_",
            "a.b",
            "a b",
            "a\tb",
            "a\nb",
            "a:b",
            "a\u{0}b",
            // multi-byte — the case a char-oriented check would get wrong
            "wt-café",
            "日本語",
            "wt-\u{7f}",
        ]
        .iter()
        .map(|s| (*s).to_string())
        // length boundary, on BYTES: 128 passes, 129 does not.
        .chain(["a".repeat(127), "a".repeat(128), "a".repeat(129)])
        // a 128-BYTE string that is only 64 chars — accepted by a char-oriented
        // length check and rejected by both of the byte-oriented ones.
        .chain(std::iter::once("é".repeat(64)))
        .collect();

        for raw in corpus {
            let generated = WtWorktreeId::new(raw.clone()).is_ok();
            let domain = WorktreeId::is_path_safe(&raw);
            assert_eq!(
                generated, domain,
                "policies disagree on {raw:?}: generated Segment says {generated}, \
                 WorktreeId::is_path_safe says {domain}"
            );
        }
    }
}
