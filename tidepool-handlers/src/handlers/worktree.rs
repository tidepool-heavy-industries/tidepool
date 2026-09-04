use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use tidepool_bridge_effects::{
    WireError, WtBranchName, WtDirtySummary, WtGitFailureReceipt, WtGitOid, WtHeadState,
    WtMergeOutcome, WtMergeRequest, WtSubmissionObservation, WtWorkingState, WtWorktreeHandle,
    WtWorktreeId, WtWorktreeReceipt, WtWorktreeSource, WtWorktreeSpec, WtWorktreeSummary,
};
use tidepool_worktree::create::{WorktreeHandle, WorktreeManager, WorktreeSource, WorktreeSpec};
use tidepool_worktree::error::{
    DirtySummary, GitFailureReceipt, WorktreeError as DomainWorktreeError,
};
use tidepool_worktree::git::GitCli;
use tidepool_worktree::id::{BranchName, WorktreeId};
use tidepool_worktree::merge::{try_merge, MergeOutcome};
#[cfg(test)]
use tidepool_worktree::registry::{WorktreeOrigin, WorktreeRecordStatus};
use tidepool_worktree::registry::{WorktreeReceipt, WorktreeRegistry, WorktreeSummary};
use tidepool_worktree::{AgentRef as WorktreePrincipal, BindingTable};
use tidepool_worktree::{HeadState, SubmissionObservation, WorkingState};

// ============================================================================
// Tag: Worktree (managed git worktrees, deliberately NOT in the
// default base_effects! row)
// ============================================================================

// WorktreeReq, WorktreeError, DescribeEffect and the EffectHandler dispatch are
// GENERATED from the `tidepool-protocol` schema — re-exported
// here so the public paths (`tidepool_handlers::WorktreeReq`,
// `tidepool_handlers::WorktreeError`) are unchanged. Only the handler struct and
// the per-verb method bodies below are hand-written.
pub use crate::generated::worktree::{WorktreeError, WorktreeReq};

#[derive(Clone)]
pub struct WorktreeHandler {
    manager: WorktreeManager,
}

impl WorktreeHandler {
    /// Compose an already-owned manager into an effect stack. This is the
    /// production actor-host path: deployment and the Haskell interpreter
    /// must share one registry/manager rather than opening parallel owners.
    #[must_use]
    pub fn from_manager(manager: WorktreeManager) -> Self {
        Self { manager }
    }

    /// `registry_root` and `worktree_root` must live OUTSIDE `source_repository`
    /// — see `tidepool-worktree/CLAUDE.md`'s "never dirty the source" rule.
    /// Fallible because opening the durable registry is (`WorktreeRegistry::open`).
    pub fn new(
        registry_root: PathBuf,
        worktree_root: PathBuf,
        source_repository: PathBuf,
    ) -> Result<Self, DomainWorktreeError> {
        // Every receipt this handler returns carries `cwd = worktree_root.
        // join(id)` (`WorktreeManager::create`), and `cwd` crosses to Haskell
        // as `Text` (`receipt_to_wire`). Decode ONCE here, at construction,
        // with a typed error — the locked "decode once at the OS boundary"
        // pattern — instead of letting a non-UTF-8 root surface as silent
        // corruption downstream on every receipt it touches.
        if camino::Utf8Path::from_path(&worktree_root).is_none() {
            return Err(DomainWorktreeError::StorageFailure {
                path: worktree_root,
                detail: "worktree_root must be valid UTF-8".to_string(),
            });
        }
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

/// Concrete-resource authority shared by the actor composition root and its
/// Worktree interpreter. Root identity is installed after unpublished-root
/// allocation; child authorization is derived from the existing durable
/// binding table and exact run/id/incarnation principal.
#[derive(Clone)]
pub struct ActorWorktreeAuthority {
    runtime: Arc<str>,
    bindings: Arc<Mutex<BindingTable>>,
    root: Arc<RwLock<Option<tidepool_repr::PrincipalId>>>,
    grants: Arc<RwLock<HashMap<tidepool_repr::PrincipalId, ActorWorktreeGrant>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActorWorktreeGrant {
    pub enumerate: bool,
    pub allocate: bool,
    pub integrate: bool,
}

impl ActorWorktreeAuthority {
    #[must_use]
    pub fn new(runtime: impl Into<Arc<str>>, bindings: Arc<Mutex<BindingTable>>) -> Self {
        Self {
            runtime: runtime.into(),
            bindings,
            root: Arc::new(RwLock::new(None)),
            grants: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn install_root(&self, principal: tidepool_repr::PrincipalId) {
        *self.root.write() = Some(principal);
    }

    pub fn install_grant(&self, principal: tidepool_repr::PrincipalId, grant: ActorWorktreeGrant) {
        self.grants.write().insert(principal, grant);
    }

    pub fn remove_grant(&self, principal: tidepool_repr::PrincipalId) {
        self.grants.write().remove(&principal);
    }

    fn is_root(&self, principal: tidepool_repr::PrincipalId) -> bool {
        // The bootstrap expression is evaluated before its unpublished root
        // receives an ActorRef, so its already-suspended continuation carries
        // the kernel's explicit SYSTEM principal. Later actor-authored turns
        // carry the installed exact root principal.
        principal == tidepool_repr::PrincipalId::SYSTEM
            || self.root.read().as_ref() == Some(&principal)
    }

    fn owns(&self, principal: tidepool_repr::PrincipalId, tree: &WorktreeId) -> bool {
        let expected = WorktreePrincipal::exact_actor(
            &self.runtime,
            principal.identity,
            principal.incarnation,
        );
        self.bindings
            .lock()
            .current(tree)
            .is_some_and(|binding| binding.agent() == &expected)
    }

    fn grant(&self, principal: tidepool_repr::PrincipalId) -> ActorWorktreeGrant {
        self.grants
            .read()
            .get(&principal)
            .copied()
            .unwrap_or_default()
    }

    fn bound_worktree(&self, principal: tidepool_repr::PrincipalId) -> Option<WorktreeId> {
        let agent = WorktreePrincipal::exact_actor(
            &self.runtime,
            principal.identity,
            principal.incarnation,
        );
        self.bindings.lock().active_for_agent(&agent).cloned()
    }
}

/// Worktree interpreter used by an actor machine. It preserves one generated
/// Worktree request decoder/implementation while adding the concrete-resource
/// membrane the general-purpose handler deliberately does not own.
#[derive(Clone)]
pub struct ActorWorktreeHandler {
    inner: WorktreeHandler,
    authority: ActorWorktreeAuthority,
}

impl ActorWorktreeHandler {
    #[must_use]
    pub fn new(inner: WorktreeHandler, authority: ActorWorktreeAuthority) -> Self {
        Self { inner, authority }
    }
}

impl ActorWorktreeHandler {
    /// Reserve exactly one named child workspace as part of an actor-owned
    /// fork admission transaction. This does not consult or confer the
    /// caller's general allocation grant.
    pub fn admit_fork_workspace(
        &mut self,
        principal: tidepool_repr::PrincipalId,
        actor_path: String,
        spec: Option<WtWorktreeSpec>,
        bound_dirty_policy: tidepool_bridge_effects::WtDirtyPolicy,
    ) -> Result<WtWorktreeHandle, WorktreeError> {
        let actor_path = tidepool_repr::ActorPath::parse(&actor_path).map_err(|error| {
            WorktreeError::WorktreeAuthorityDenied(format!("invalid actor path: {error}"))
        })?;
        let spec = match spec {
            Some(spec) => {
                let spec = spec_from_wire(spec)?;
                if !self.authority.is_root(principal) {
                    match &spec.source {
                        WorktreeSource::Worktree(tree) if self.authority.owns(principal, tree) => {}
                        WorktreeSource::Worktree(tree) => {
                            return Err(WorktreeError::WorktreeUnauthorized(worktree_id_to_wire(
                                tree,
                            )));
                        }
                        WorktreeSource::CurrentRepository | WorktreeSource::Ref(_) => {
                            return Err(WorktreeError::WorktreeAuthorityDenied(
                                "non-root context forks must derive worktrees from their bound checkout"
                                    .into(),
                            ));
                        }
                    }
                }
                spec
            }
            None => {
                let Some(source) = self.authority.bound_worktree(principal) else {
                    return Err(WorktreeError::WorktreeAuthorityDenied(
                        "boundHead requires one active bound worktree".into(),
                    ));
                };
                WorktreeSpec {
                    source: WorktreeSource::Worktree(source),
                    label: actor_path.to_string(),
                    dirty_policy: dirty_policy_from_wire(bound_dirty_policy),
                }
            }
        };
        self.inner
            .manager
            .create_for_actor_path(&spec, &actor_path)
            .map(|handle| handle_to_wire(&handle))
            .map_err(error_to_wire)
    }
}

macro_rules! actor_worktree_facade {
    ($name:ident) => {
        #[derive(Clone)]
        pub struct $name {
            inner: ActorWorktreeHandler,
        }

        impl $name {
            #[must_use]
            pub fn new(inner: ActorWorktreeHandler) -> Self {
                Self { inner }
            }

            fn delegate(
                &mut self,
                request: WorktreeReq,
                cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
            ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
                tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, request, cx)
            }
        }
    };
}

actor_worktree_facade!(ActorBoundWorktreeHandler);
actor_worktree_facade!(ActorWorktreeRegistryHandler);
actor_worktree_facade!(ActorWorktreeAllocationHandler);
actor_worktree_facade!(ActorWorktreeIntegrationHandler);

impl ActorBoundWorktreeHandler {
    pub(crate) fn bound_worktree_get(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeBound, cx)
    }

    pub(crate) fn bound_worktree_lookup(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        tree: WtWorktreeId,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeLookup(tree), cx)
    }

    pub(crate) fn bound_worktree_branch_of(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        tree: WtWorktreeId,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeBranchOf(tree), cx)
    }

    pub(crate) fn bound_worktree_head_of(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        tree: WtWorktreeId,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeHeadOf(tree), cx)
    }

    pub(crate) fn bound_worktree_observe_submission(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        tree: WtWorktreeId,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeObserveSubmission(tree), cx)
    }
}

impl ActorWorktreeRegistryHandler {
    pub(crate) fn worktree_registry_lookup(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        tree: WtWorktreeId,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeLookup(tree), cx)
    }

    pub(crate) fn worktree_registry_list(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeList, cx)
    }

    pub(crate) fn worktree_registry_query(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        present: Option<bool>,
        branch_prefix: Option<String>,
        created_after: Option<i64>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(
            WorktreeReq::WorktreeListMatching(present, branch_prefix, created_after),
            cx,
        )
    }
}

impl ActorWorktreeAllocationHandler {
    pub(crate) fn worktree_allocation_create(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        spec: WtWorktreeSpec,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeCreate(spec), cx)
    }

    pub(crate) fn worktree_allocation_create_for_actor_path(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        spec: WtWorktreeSpec,
        actor_path: String,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(
            WorktreeReq::WorktreeCreateForActorPath(spec, actor_path),
            cx,
        )
    }

    pub(crate) fn worktree_allocation_create_from_bound_for_actor_path(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        dirty_policy: tidepool_bridge_effects::WtDirtyPolicy,
        actor_path: String,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(
            WorktreeReq::WorktreeCreateFromBoundForActorPath(dirty_policy, actor_path),
            cx,
        )
    }
}

impl ActorWorktreeIntegrationHandler {
    pub(crate) fn worktree_integration_try_merge(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        request: tidepool_bridge_effects::WtMergeRequest,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeTryMerge(request), cx)
    }
}

impl tidepool_effect::dispatch::EffectHandler<tidepool_mcp::CapturedOutput>
    for ActorWorktreeHandler
{
    type Request = WorktreeReq;

    fn handle(
        &mut self,
        req: WorktreeReq,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        let principal = cx.principal();
        if self.authority.is_root(principal) {
            return tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, req, cx);
        }

        let grant = self.authority.grant(principal);
        let permitted_tree = match &req {
            WorktreeReq::WorktreeBound => {
                let Some(id) = self.authority.bound_worktree(principal) else {
                    return cx.respond(Err::<(), _>(WorktreeError::WorktreeAuthorityDenied(
                        "this actor has no active bound worktree".into(),
                    )));
                };
                let result = self
                    .inner
                    .manager
                    .lookup(&id)
                    .map_err(error_to_wire)
                    .and_then(|handle| {
                        handle
                            .map(|handle| handle_to_wire(&handle))
                            .ok_or_else(|| never_registered(&worktree_id_to_wire(&id)))
                    });
                return cx.respond(result);
            }
            WorktreeReq::WorktreeLookup(id)
            | WorktreeReq::WorktreeBranchOf(id)
            | WorktreeReq::WorktreeHeadOf(id)
            | WorktreeReq::WorktreeObserveSubmission(id) => Some(id),
            WorktreeReq::WorktreeCreateForActorPath(spec, path) if grant.allocate => {
                return cx.respond(
                    self.inner
                        .worktree_create_for_actor_path(spec.clone(), path.clone()),
                );
            }
            WorktreeReq::WorktreeCreateFromBoundForActorPath(dirty_policy, path)
                if grant.allocate =>
            {
                let Some(source) = self.authority.bound_worktree(principal) else {
                    return cx.respond(Err::<(), _>(WorktreeError::WorktreeAuthorityDenied(
                        "boundHead requires one active bound worktree".into(),
                    )));
                };
                let spec = WorktreeSpec {
                    source: WorktreeSource::Worktree(source),
                    label: path.clone(),
                    dirty_policy: dirty_policy_from_wire(*dirty_policy),
                };
                let actor_path = match tidepool_repr::ActorPath::parse(path) {
                    Ok(path) => path,
                    Err(error) => {
                        return cx.respond(Err::<(), _>(WorktreeError::WorktreeAuthorityDenied(
                            format!("invalid actor path: {error}"),
                        )));
                    }
                };
                let result = self
                    .inner
                    .manager
                    .create_for_actor_path(&spec, &actor_path)
                    .map(|handle| handle_to_wire(&handle))
                    .map_err(error_to_wire);
                return cx.respond(result);
            }
            WorktreeReq::WorktreeCreate(_) if grant.allocate => {
                return tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, req, cx);
            }
            WorktreeReq::WorktreeList | WorktreeReq::WorktreeListMatching(..)
                if grant.enumerate =>
            {
                return tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, req, cx);
            }
            WorktreeReq::WorktreeTryMerge(..) if grant.integrate => {
                return tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, req, cx);
            }
            WorktreeReq::WorktreeCreate(_)
            | WorktreeReq::WorktreeCreateForActorPath(..)
            | WorktreeReq::WorktreeCreateFromBoundForActorPath(..)
            | WorktreeReq::WorktreeList
            | WorktreeReq::WorktreeListMatching(..)
            | WorktreeReq::WorktreeTryMerge(..) => None,
        };
        if let Some(wire_id) = permitted_tree {
            let id = match worktree_id_from_wire(wire_id) {
                Ok(id) => id,
                Err(error) => return cx.respond(Err::<(), _>(error)),
            };
            if self.authority.owns(principal, &id) {
                return tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, req, cx);
            }
            return cx.respond(Err::<(), _>(WorktreeError::WorktreeUnauthorized(
                wire_id.clone(),
            )));
        }
        cx.respond(Err::<(), _>(WorktreeError::WorktreeAuthorityDenied(
            "this actor may inspect its bound worktree but may not allocate, enumerate, or merge managed worktrees".into(),
        )))
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
    branch_name_from_wire, branch_name_to_wire, dirty_policy_from_wire, git_oid_to_wire,
    git_ref_from_wire, git_ref_to_wire, in_progress_kind_to_wire, worktree_id_to_wire,
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

fn head_state_to_wire(head: &HeadState) -> WtHeadState {
    match head {
        HeadState::OnBranch { branch, oid } => WtHeadState::OnBranch {
            branch: branch_name_to_wire(branch),
            oid: git_oid_to_wire(oid),
        },
        HeadState::Detached { oid } => WtHeadState::Detached {
            oid: git_oid_to_wire(oid),
        },
    }
}

fn working_state_to_wire(state: &WorkingState) -> WtWorkingState {
    WtWorkingState {
        changes: dirty_summary_to_wire(&state.changes),
        operation: state.operation.map(in_progress_kind_to_wire),
    }
}

fn submission_observation_to_wire(observation: &SubmissionObservation) -> WtSubmissionObservation {
    WtSubmissionObservation {
        observed_worktree_id: worktree_id_to_wire(&observation.worktree_id),
        base_head: git_oid_to_wire(&observation.base_head),
        committed_paths: observation.committed_paths.clone(),
        submitted_head: head_state_to_wire(&observation.submitted_head),
        working_state: working_state_to_wire(&observation.working_state),
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
        // `cwd` is `worktree_root.join(id)`: `worktree_root` is checked
        // UTF-8 once at `WorktreeHandler::new` and `id` is ASCII-safe by
        // construction (`WorktreeId::is_path_safe`) — so this is UTF-8 by
        // construction, not a per-call fallible boundary.
        cwd: camino::Utf8Path::from_path(&r.cwd)
            .unwrap_or_else(|| {
                panic!(
                    "invariant violated: worktree cwd is not valid UTF-8 despite a \
                     UTF-8-checked root and an ASCII-safe id: {}",
                    r.cwd.display()
                )
            })
            .as_str()
            .to_string(),
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

fn merge_outcome_to_wire(o: MergeOutcome) -> WtMergeOutcome {
    match o {
        MergeOutcome::AlreadyContained { source, target } => {
            WtMergeOutcome::AlreadyContained(git_oid_to_wire(&source), git_oid_to_wire(&target))
        }
        MergeOutcome::FastForwarded {
            source,
            before,
            after,
        } => WtMergeOutcome::FastForwarded(
            git_oid_to_wire(&source),
            git_oid_to_wire(&before),
            git_oid_to_wire(&after),
        ),
        MergeOutcome::CreatedMergeCommit {
            source,
            before,
            commit,
        } => WtMergeOutcome::CreatedMergeCommit(
            git_oid_to_wire(&source),
            git_oid_to_wire(&before),
            git_oid_to_wire(&commit),
        ),
        MergeOutcome::ManualGitRequired {
            source,
            target,
            reason,
            paths,
        } => WtMergeOutcome::ManualGitRequired(
            git_oid_to_wire(&source),
            git_oid_to_wire(&target),
            reason,
            paths,
        ),
    }
}

/// Total map from the domain `WorktreeError` (`tidepool-worktree/src/error.rs`)
/// to the wire `WorktreeError` (generated from the schema's `errors` block) —
/// ten variants on the wire side, twelve on the domain side. No wildcard arm
/// below, so a domain variant added without an explicit wire mapping fails
/// this match's exhaustiveness check at compile time — but the two
/// persistence-versioning variants (`JournalBelowFloor`/`JournalFutureVersion`,
/// added by #21) fold onto the existing wire `StorageFailure` rather than
/// widening the schema: the schema's `errors` block is generated Haskell-visible
/// surface (`tidepool-protocol`, regenerated through the GHC toolchain), out
/// of this lane's boundary, and a below-floor/future-version journal read is
/// exactly the same kind of fact `StorageFailure` already carries — "Tidepool's
/// own durable storage failed" — just with a richer, typed Rust-side reason
/// than the wire's plain `(path, detail)` pair distinguishes.
pub(crate) fn error_to_wire(e: DomainWorktreeError) -> WorktreeError {
    match e {
        DomainWorktreeError::JournalBelowFloor { path, found, floor } => {
            WorktreeError::StorageFailure(
                path.to_string_lossy().into_owned(),
                format!(
                    "event journal version {found} is below the floor this build still supports \
                 ({floor}) — archive or delete it and start a fresh journal, or read it with an \
                 older tidepool build that still supports version {found}"
                ),
            )
        }
        DomainWorktreeError::JournalFutureVersion {
            path,
            found,
            current,
        } => WorktreeError::StorageFailure(
            path.to_string_lossy().into_owned(),
            format!(
                "event journal version {found} is newer than this build supports (current \
                 {current}) — rebuild against a newer tidepool, or archive/delete the journal \
                 and start fresh"
            ),
        ),
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
        DomainWorktreeError::SubmissionUnstable(id) => {
            WorktreeError::SubmissionUnstable(worktree_id_to_wire(&id))
        }
        DomainWorktreeError::WorktreeUnauthorized(id) => {
            WorktreeError::WorktreeUnauthorized(worktree_id_to_wire(&id))
        }
        DomainWorktreeError::WorktreeAuthorityDenied(detail) => {
            WorktreeError::WorktreeAuthorityDenied(detail)
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

    pub(crate) fn worktree_create_for_actor_path(
        &mut self,
        spec: WtWorktreeSpec,
        actor_path: String,
    ) -> Result<WtWorktreeHandle, WorktreeError> {
        let domain_spec = spec_from_wire(spec)?;
        let actor_path = tidepool_repr::ActorPath::parse(&actor_path).map_err(|error| {
            WorktreeError::WorktreeAuthorityDenied(format!("invalid actor path: {error}"))
        })?;
        let handle = self
            .manager
            .create_for_actor_path(&domain_spec, &actor_path)
            .map_err(error_to_wire)?;
        Ok(handle_to_wire(&handle))
    }

    pub(crate) fn worktree_create_from_bound_for_actor_path(
        &mut self,
        _dirty_policy: tidepool_bridge_effects::WtDirtyPolicy,
        _actor_path: String,
    ) -> Result<WtWorktreeHandle, WorktreeError> {
        Err(WorktreeError::WorktreeAuthorityDenied(
            "boundHead is available only through an actor-scoped Worktree interpreter".into(),
        ))
    }

    /// `Ok(None)` from `WorktreeManager::lookup` (the id was NEVER
    /// registered — a typo-shaped miss) must stay distinguishable from
    /// `Err(WorktreeLost)` (registered, then gone from disk — data loss); see
    /// `tidepool-worktree/src/registry.rs`'s `WorktreeRegistry::get` doc.
    /// `Ok(None)` is spelled here as the wire `WorktreeNotRegistered`
    /// variant (`never_registered` above), which the `errors` block carries
    /// specifically to preserve this distinction — never collapsed onto
    /// `WorktreeLost`'s tag, which would erase that visible distinction.
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

    pub(crate) fn worktree_bound(&mut self) -> Result<WtWorktreeHandle, WorktreeError> {
        Err(WorktreeError::WorktreeAuthorityDenied(
            "boundWorktree is available only through an actor-scoped Worktree interpreter".into(),
        ))
    }

    pub(crate) fn worktree_list(&mut self) -> Result<Vec<WtWorktreeSummary>, WorktreeError> {
        let summaries = self.manager.list().map_err(error_to_wire)?;
        Ok(summaries.iter().map(summary_to_wire).collect())
    }

    pub(crate) fn worktree_list_matching(
        &mut self,
        present: Option<bool>,
        branch_prefix: Option<String>,
        created_after: Option<i64>,
    ) -> Result<Vec<WtWorktreeSummary>, WorktreeError> {
        let summaries = self.manager.list().map_err(error_to_wire)?;
        Ok(summaries
            .iter()
            .filter(|summary| present.is_none_or(|expected| summary.present == expected))
            .filter(|summary| {
                branch_prefix
                    .as_deref()
                    .is_none_or(|prefix| summary.receipt.branch.as_str().starts_with(prefix))
            })
            .filter(|summary| {
                created_after.is_none_or(|timestamp| summary.receipt.created_at_ms > timestamp)
            })
            .map(summary_to_wire)
            .collect())
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
    /// last-observed baseline. A resident spanning loop iterations compares this
    /// against its own checkpointed head to close the gap where HEAD
    /// moved after one monitor loop iteration unregistered and before the next
    /// registered; returning either recorded value would leave that gap
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

    pub(crate) fn worktree_observe_submission(
        &mut self,
        tree_id: WtWorktreeId,
    ) -> Result<WtSubmissionObservation, WorktreeError> {
        let id = worktree_id_from_wire(&tree_id)?;
        let handle = self
            .manager
            .lookup(&id)
            .map_err(error_to_wire)?
            .ok_or_else(|| never_registered(&tree_id))?;
        let observation = self
            .manager
            .observe_submission(&handle)
            .map_err(error_to_wire)?;
        Ok(submission_observation_to_wire(&observation))
    }

    /// The one narrow, deliberate workflow primitive (see
    /// `tidepool-worktree/src/merge.rs`): merge `branch` into the worktree
    /// `target_worktree` names, through `tidepool_worktree::merge::try_merge`
    /// — the same typed conflict-vs-failure classification and abort-before-
    /// return discipline every caller gets, instead of each authored harness
    /// re-deriving it over raw `Exec`.
    pub(crate) fn worktree_try_merge(
        &mut self,
        request: WtMergeRequest,
    ) -> Result<WtMergeOutcome, WorktreeError> {
        let id = worktree_id_from_wire(&request.target_worktree)?;
        let handle = self
            .manager
            .lookup(&id)
            .map_err(error_to_wire)?
            .ok_or_else(|| never_registered(&request.target_worktree))?;
        let source = tidepool_worktree::GitOid::from_raw(request.source_head.raw);
        let source_branch = request.source_branch.as_ref().map(branch_name_from_wire);
        let outcome = try_merge(
            self.manager.git(),
            handle.cwd(),
            &source,
            source_branch.as_ref(),
            &request.merge_message,
        )
        .map_err(error_to_wire)?;
        Ok(merge_outcome_to_wire(outcome))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The wire enums/newtypes these tables construct directly. The module body
    // no longer names them — its mechanical conversions are generated — but the
    // tables below still build wire values by hand, which is the point: they
    // assert against literals, not against the conversions under test.
    use tidepool_bridge_effects::{WtDirtyPolicy, WtGitRef, WtInProgressKind, WtWorktreeSource};
    use tidepool_worktree::create::DirtyPolicy;
    use tidepool_worktree::error::InProgressKind;
    use tidepool_worktree::id::{GitOid, GitRef};

    #[test]
    fn actor_worktree_authority_is_exact_to_resource_and_incarnation() {
        let storage = tempfile::tempdir().unwrap();
        let bindings = Arc::new(Mutex::new(BindingTable::open(storage.path()).unwrap()));
        let authority = ActorWorktreeAuthority::new("run-1", Arc::clone(&bindings));
        let root = tidepool_repr::PrincipalId::new(1, 1);
        let worker = tidepool_repr::PrincipalId::new(2, 3);
        let tree = WorktreeId::from_raw("worker-tree");
        authority.install_root(root);

        let worker_principal = WorktreePrincipal::exact_actor("run-1", 2, 3);
        let binding = bindings.lock().bind(&tree, &worker_principal, 1).unwrap();

        assert!(authority.is_root(root));
        assert!(authority.is_root(tidepool_repr::PrincipalId::SYSTEM));
        assert!(authority.owns(worker, &tree));
        assert!(!authority.owns(tidepool_repr::PrincipalId::new(2, 4), &tree));
        assert!(!authority.owns(tidepool_repr::PrincipalId::new(3, 3), &tree));
        assert!(!authority.owns(worker, &WorktreeId::from_raw("another-tree")));

        binding.release(&mut bindings.lock()).unwrap();
        assert!(!authority.owns(worker, &tree));
    }

    #[test]
    fn fork_admission_allocates_one_exact_named_workspace_without_a_general_grant() {
        let repository = tidepool_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "seed\n", "seed")
            .unwrap();
        let storage = tempfile::tempdir().unwrap();
        let registry = WorktreeRegistry::open(storage.path().join("registry")).unwrap();
        let manager = WorktreeManager::new(
            GitCli::new(),
            registry,
            storage.path().join("worktrees"),
            repository.path(),
        );
        let bindings = Arc::new(Mutex::new(
            BindingTable::open(storage.path().join("bindings")).unwrap(),
        ));
        let authority = ActorWorktreeAuthority::new("run-1", bindings);
        let root = tidepool_repr::PrincipalId::new(1, 1);
        authority.install_root(root);
        let mut handler =
            ActorWorktreeHandler::new(WorktreeHandler::from_manager(manager), authority);

        let admitted = handler
            .admit_fork_workspace(
                root,
                "campaign/group/leaf".into(),
                Some(WtWorktreeSpec {
                    spec_source: WtWorktreeSource::SourceCurrentRepository,
                    spec_label: "ignored-by-path-projection".into(),
                    spec_dirty_policy: WtDirtyPolicy::RequireClean,
                }),
                WtDirtyPolicy::RequireClean,
            )
            .unwrap();

        assert_eq!(
            admitted.handle_receipt.branch.raw,
            "shoal/campaign/group/branches/leaf"
        );
    }

    #[test]
    fn worktree_query_filters_in_the_registry_handler() {
        let repository = tidepool_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "seed\n", "seed")
            .unwrap();
        let storage = tempfile::tempdir().unwrap();
        let registry = WorktreeRegistry::open(storage.path().join("registry")).unwrap();
        let manager = WorktreeManager::new(
            GitCli::new(),
            registry,
            storage.path().join("worktrees"),
            repository.path(),
        );
        let mut handler = WorktreeHandler::from_manager(manager);
        let created = handler
            .worktree_create(WtWorktreeSpec {
                spec_source: WtWorktreeSource::SourceCurrentRepository,
                spec_label: "query-target".into(),
                spec_dirty_policy: WtDirtyPolicy::RequireClean,
            })
            .unwrap();
        let receipt = created.handle_receipt;

        let matched = handler
            .worktree_list_matching(
                Some(true),
                Some(receipt.branch.raw.clone()),
                Some(receipt.created_at - 1),
            )
            .unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].summary_receipt.tree_id, receipt.tree_id);

        assert!(handler
            .worktree_list_matching(Some(false), None, None)
            .unwrap()
            .is_empty());
        assert!(handler
            .worktree_list_matching(None, None, Some(receipt.created_at))
            .unwrap()
            .is_empty());
    }

    /// `cwd` on every receipt this handler returns is `worktree_root.join(id)`
    /// and crosses to Haskell as `Text` (`receipt_to_wire`) — a non-UTF-8
    /// `worktree_root` must be rejected here, at construction, as a typed
    /// error rather than silently corrupting every receipt this handler ever
    /// returns.
    #[cfg(unix)]
    #[test]
    fn worktree_handler_new_rejects_non_utf8_worktree_root() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let registry_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        // `0x80` alone is not a valid UTF-8 lead byte.
        let bad_name = OsString::from_vec(vec![b'w', b't', 0x80]);
        let bad_root = registry_dir.path().join(PathBuf::from(bad_name));

        match WorktreeHandler::new(
            registry_dir.path().to_path_buf(),
            bad_root,
            source_dir.path().to_path_buf(),
        ) {
            Ok(_) => panic!("a non-UTF-8 worktree_root must be rejected, not silently accepted"),
            Err(err) => assert!(
                matches!(err, DomainWorktreeError::StorageFailure { .. }),
                "{err:?}"
            ),
        }
    }

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
                "submission_unstable",
                DomainWorktreeError::SubmissionUnstable(WorktreeId::from_raw("wt-moving")),
                WorktreeError::SubmissionUnstable(WtWorktreeId {
                    raw: "wt-moving".to_string(),
                }),
            ),
            (
                "worktree_unauthorized",
                DomainWorktreeError::WorktreeUnauthorized(WorktreeId::from_raw("wt-private")),
                WorktreeError::WorktreeUnauthorized(WtWorktreeId {
                    raw: "wt-private".to_string(),
                }),
            ),
            (
                "worktree_authority_denied",
                DomainWorktreeError::WorktreeAuthorityDenied("allocation is owner-only".into()),
                WorktreeError::WorktreeAuthorityDenied("allocation is owner-only".into()),
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

    /// Persistence versioning (#21)'s two journal-version domain variants
    /// fold onto the wire's existing `StorageFailure` — see `error_to_wire`'s
    /// own doc for why they don't widen the (Haskell-visible, generated)
    /// wire schema. Not in the table above only because their expected
    /// wire message is built from a `format!`, not a literal.
    #[test]
    fn journal_version_errors_fold_onto_wire_storage_failure() {
        let below = error_to_wire(DomainWorktreeError::JournalBelowFloor {
            path: PathBuf::from("/run/segment-0.jsonl"),
            found: 0,
            floor: 1,
        });
        match below {
            WorktreeError::StorageFailure(path, detail) => {
                assert_eq!(path, "/run/segment-0.jsonl");
                assert!(detail.contains('0'), "{detail}");
                assert!(detail.contains('1'), "{detail}");
            }
            other => panic!("expected StorageFailure, got {other:?}"),
        }

        let future = error_to_wire(DomainWorktreeError::JournalFutureVersion {
            path: PathBuf::from("/run/segment-0.jsonl"),
            found: 9999,
            current: 1,
        });
        match future {
            WorktreeError::StorageFailure(path, detail) => {
                assert_eq!(path, "/run/segment-0.jsonl");
                assert!(detail.contains("9999"), "{detail}");
            }
            other => panic!("expected StorageFailure, got {other:?}"),
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
