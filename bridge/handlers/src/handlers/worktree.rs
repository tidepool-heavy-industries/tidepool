use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tidepool_atomic_write::DirectoryAnchor;

use parking_lot::{Mutex, RwLock};

use exomonad_worktree::create::{WorktreeHandle, WorktreeManager, WorktreeSource, WorktreeSpec};
use exomonad_worktree::error::{
    DirtySummary, GitFailureReceipt, WorktreeError as DomainWorktreeError,
};
use exomonad_worktree::git::GitCli;
#[cfg(test)]
use exomonad_worktree::id::BranchName;
use exomonad_worktree::id::WorktreeId;
use exomonad_worktree::merge::{try_merge, MergeOutcome};
#[cfg(test)]
use exomonad_worktree::registry::{WorktreeOrigin, WorktreeRecordStatus};
use exomonad_worktree::registry::{WorktreeReceipt, WorktreeRegistry, WorktreeSummary};
use exomonad_worktree::{AgentRef as WorktreePrincipal, BindingTable, WorkspaceAccess};
use exomonad_worktree::{HeadState, SubmissionObservation, WorkingState};
use tidepool_bridge_effects::{
    WireError, WtBranchName, WtDirtySummary, WtGitFailureReceipt, WtGitOid, WtHeadState,
    WtMergeOutcome, WtMergeRequest, WtSubmissionObservation, WtWorkingState, WtWorktreeHandle,
    WtWorktreeId, WtWorktreeReceipt, WtWorktreeSource, WtWorktreeSpec, WtWorktreeSummary,
};

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

    /// The registry and `worktree_root` must live OUTSIDE `source_repository`
    /// — see `exomonad/worktree/CLAUDE.md`'s "never dirty the source" rule.
    /// Fallible because opening the durable registry is (`WorktreeRegistry::open`).
    pub fn new(
        storage: &DirectoryAnchor,
        registry_relative: impl AsRef<Path>,
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
        let registry = WorktreeRegistry::open(storage, registry_relative)?;
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
/// Worktree interpreter. Host-issued grants and durable checkout bindings
/// authorize exact run/id/incarnation principals independently of model roles.
#[derive(Clone)]
pub struct ActorWorktreeAuthority {
    runtime: Arc<str>,
    bindings: Arc<Mutex<BindingTable>>,
    grants: Arc<RwLock<HashMap<tidepool_repr::PrincipalId, ActorWorktreeGrant>>>,
    workspace_grants: Arc<RwLock<HashMap<String, (WorktreeId, WorkspaceAccess)>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorWorktreeGrant {
    Repository,
    /// Repository inspection for the operator workbench, which has no
    /// writable checkout of its own.
    RepositoryReadOnly,
    Bound {
        enumerate: bool,
        allocate: bool,
        integrate: bool,
    },
}

impl Default for ActorWorktreeGrant {
    fn default() -> Self {
        Self::Bound {
            enumerate: false,
            allocate: false,
            integrate: false,
        }
    }
}

impl ActorWorktreeAuthority {
    #[must_use]
    pub fn new(runtime: impl Into<Arc<str>>, bindings: Arc<Mutex<BindingTable>>) -> Self {
        Self {
            runtime: runtime.into(),
            bindings,
            grants: Arc::new(RwLock::new(HashMap::new())),
            workspace_grants: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn install_grant(&self, principal: tidepool_repr::PrincipalId, grant: ActorWorktreeGrant) {
        self.grants.write().insert(principal, grant);
    }

    pub fn remove_grant(&self, principal: tidepool_repr::PrincipalId) {
        self.grants.write().remove(&principal);
    }

    fn has_repository_access(&self, principal: tidepool_repr::PrincipalId) -> bool {
        matches!(
            self.grant(principal),
            ActorWorktreeGrant::Repository | ActorWorktreeGrant::RepositoryReadOnly
        )
    }

    fn owns(&self, principal: tidepool_repr::PrincipalId, tree: &WorktreeId) -> bool {
        let expected = WorktreePrincipal::exact_actor(
            &self.runtime,
            principal.identity,
            principal.incarnation,
        );
        let bindings = self.bindings.lock();
        let current = bindings.membership(tree, &expected);
        let owned = current.is_some();
        if !owned {
            tracing::warn!(worktree = %tree, expected_principal = %expected,
                actual_principal = ?current.map(|binding| binding.agent()),
                binding_ready = current.is_some(), "worktree custody denied");
        }
        owned
    }

    /// The host-installed resource grant for this exact actor incarnation.
    pub fn grant(&self, principal: tidepool_repr::PrincipalId) -> ActorWorktreeGrant {
        // Bootstrap runs before actor identity is allocated. Authored turns
        // carry their exact actor principal and use only its installed grant.
        if principal == tidepool_repr::PrincipalId::SYSTEM {
            return ActorWorktreeGrant::Repository;
        }
        self.grants
            .read()
            .get(&principal)
            .copied()
            .unwrap_or_default()
    }

    /// Issue an opaque run capability from the caller's exact attachment.
    /// Capabilities retain backing identity independently of their issuing actor.
    pub fn issue_workspace(
        &self,
        principal: tidepool_repr::PrincipalId,
        tree: &WorktreeId,
        requested: WorkspaceAccess,
    ) -> Result<String, DomainWorktreeError> {
        let available = self.workspace_access(principal, tree).or_else(|| {
            self.has_repository_access(principal).then(|| {
                if self.grant(principal) == ActorWorktreeGrant::Repository {
                    WorkspaceAccess::ReadWrite
                } else {
                    WorkspaceAccess::ReadOnly
                }
            })
        });
        if !available.is_some_and(|access| access.permits(requested)) {
            return Err(DomainWorktreeError::WorktreeUnauthorized(tree.clone()));
        }
        let mut grants = self.workspace_grants.write();
        if let Some((token, _)) = grants
            .iter()
            .find(|(_, (issued_tree, access))| issued_tree == tree && *access == requested)
        {
            return Ok(token.clone());
        }
        let token = uuid::Uuid::new_v4().to_string();
        grants.insert(token.clone(), (tree.clone(), requested));
        Ok(token)
    }

    pub fn workspace_capability(
        &self,
        token: &str,
    ) -> Result<(WorktreeId, WorkspaceAccess), DomainWorktreeError> {
        self.workspace_grants
            .read()
            .get(token)
            .cloned()
            .ok_or_else(|| {
                DomainWorktreeError::WorktreeAuthorityDenied(
                    "unknown or expired workspace capability".into(),
                )
            })
    }

    /// An opaque grant can only be attenuated; a registered id is not a grant.
    pub fn resolve_workspace(
        &self,
        token: &str,
        requested: WorkspaceAccess,
    ) -> Result<WorktreeId, DomainWorktreeError> {
        let grants = self.workspace_grants.read();
        let (tree, available) = grants.get(token).ok_or_else(|| {
            DomainWorktreeError::WorktreeAuthorityDenied(
                "unknown or expired workspace capability".into(),
            )
        })?;
        if !available.permits(requested) {
            return Err(DomainWorktreeError::WorktreeAuthorityDenied(
                "workspace capability cannot widen read-only access".into(),
            ));
        }
        Ok(tree.clone())
    }

    pub fn workspace_access(
        &self,
        principal: tidepool_repr::PrincipalId,
        tree: &WorktreeId,
    ) -> Option<WorkspaceAccess> {
        let agent = WorktreePrincipal::exact_actor(
            &self.runtime,
            principal.identity,
            principal.incarnation,
        );
        self.bindings
            .lock()
            .membership(tree, &agent)
            .map(|binding| binding.access())
    }

    /// The worktree this principal currently holds custody of, if any.
    /// Each exact actor has one primary attachment; this is where a command raised by an
    /// actor with no sandbox of its own runs.
    pub fn bound_worktree(&self, principal: tidepool_repr::PrincipalId) -> Option<WorktreeId> {
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

/// Source authority resolved for one named fork. The host may await native
/// workspace admission before consuming this request; Git work does not require
/// holding the actor handler's shared lock.
pub struct AuthorizedForkWorkspace {
    manager: WorktreeManager,
    spec: WorktreeSpec,
}

impl AuthorizedForkWorkspace {
    pub fn source(&self) -> &exomonad_worktree::WorktreeSource {
        &self.spec.source
    }

    pub fn prepare_source(
        &self,
    ) -> Result<exomonad_worktree::PreparedSourceWorktree, WorktreeError> {
        self.manager
            .prepare_inherited_source(&self.spec.source)
            .map_err(error_to_wire)
    }

    pub fn materialize_committed(self) -> Result<WtWorktreeHandle, WorktreeError> {
        self.manager
            .create_committed_fork(&self.spec.source)
            .map(|handle| handle_to_wire(&handle))
            .map_err(error_to_wire)
    }

    pub fn materialize(self) -> Result<WtWorktreeHandle, WorktreeError> {
        self.manager
            .create(&self.spec)
            .map(|handle| handle_to_wire(&handle))
            .map_err(error_to_wire)
    }
}

impl ActorWorktreeHandler {
    #[must_use]
    pub fn new(inner: WorktreeHandler, authority: ActorWorktreeAuthority) -> Self {
        Self { inner, authority }
    }
}

impl ActorWorktreeHandler {
    fn authorize_source(
        &self,
        principal: tidepool_repr::PrincipalId,
        source: &WorktreeSource,
    ) -> Result<(), WorktreeError> {
        if !self.authority.has_repository_access(principal) {
            match source {
                WorktreeSource::Worktree(tree) if self.authority.owns(principal, tree) => {}
                WorktreeSource::Worktree(tree) => {
                    return Err(WorktreeError::WorktreeUnauthorized(worktree_id_to_wire(
                        tree,
                    )));
                }
                WorktreeSource::Ref(_) => {
                    // Linked worktrees intentionally share committed objects
                    // and refs. A bound worker can review a child's commit
                    // without changing its own checkout or reading root dirt.
                    let bound = self.authority.bound_worktree(principal).ok_or_else(|| {
                        WorktreeError::WorktreeAuthorityDenied(
                            "committed-ref seeds require an active bound worktree".into(),
                        )
                    })?;
                    if self
                        .inner
                        .manager
                        .lookup(&bound)
                        .map_err(error_to_wire)?
                        .is_none()
                    {
                        return Err(WorktreeError::WorktreeUnauthorized(worktree_id_to_wire(
                            &bound,
                        )));
                    }
                }
                WorktreeSource::CurrentRepository => {
                    return Err(WorktreeError::WorktreeAuthorityDenied(
                                "projectHead requires repository authority. In a bound actor use currentCheckout (or solTask/componentLead without From); use atRef (GitRef commit) for exact committed source."
                                    .into(),
                            ));
                }
            }
        }
        Ok(())
    }

    pub fn authorize_committed_fork(
        &self,
        principal: tidepool_repr::PrincipalId,
        source: WtWorktreeSource,
    ) -> Result<AuthorizedForkWorkspace, WorktreeError> {
        let source = worktree_source_from_wire(source)?;
        self.authorize_source(principal, &source)?;
        Ok(AuthorizedForkWorkspace {
            manager: self.inner.manager.clone(),
            spec: WorktreeSpec {
                source,
                label: String::new(),
                dirty_policy: exomonad_worktree::DirtyPolicy::RequireClean,
            },
        })
    }

    /// Reserve one child workspace as part of an actor-owned
    /// fork admission transaction. This does not consult or confer the
    /// caller's general allocation grant.
    pub fn admit_fork_workspace(
        &mut self,
        principal: tidepool_repr::PrincipalId,
        spec: Option<WtWorktreeSpec>,
        bound_dirty_policy: tidepool_bridge_effects::WtDirtyPolicy,
    ) -> Result<WtWorktreeHandle, WorktreeError> {
        self.authorize_fork_workspace(principal, spec, bound_dirty_policy)?
            .materialize()
    }

    pub fn authorize_fork_workspace(
        &self,
        principal: tidepool_repr::PrincipalId,
        spec: Option<WtWorktreeSpec>,
        bound_dirty_policy: tidepool_bridge_effects::WtDirtyPolicy,
    ) -> Result<AuthorizedForkWorkspace, WorktreeError> {
        let spec = match spec {
            Some(spec) => {
                let spec = spec_from_wire(spec)?;
                self.authorize_source(principal, &spec.source)?;
                spec
            }
            None => {
                let source = match self.authority.bound_worktree(principal) {
                    Some(source) => WorktreeSource::Worktree(source),
                    None if self.authority.has_repository_access(principal) => {
                        WorktreeSource::CurrentRepository
                    }
                    None => return Err(WorktreeError::WorktreeAuthorityDenied(
                        "currentCheckout requires an active bound checkout or repository authority; ask the parent to restore your checkout binding".into(),
                    )),
                };
                WorktreeSpec {
                    source,
                    label: String::new(),
                    dirty_policy: dirty_policy_from_wire(bound_dirty_policy),
                }
            }
        };
        Ok(AuthorizedForkWorkspace {
            manager: self.inner.manager.clone(),
            spec,
        })
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
    pub(crate) fn bound_workspace_get(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.delegate(WorktreeReq::WorktreeCurrentWorkspace, cx)
    }

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

    fn prepare(
        &mut self,
        req: WorktreeReq,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> Result<tidepool_effect::dispatch::EffectDispatch, tidepool_effect::error::EffectError>
    {
        use tidepool_effect::dispatch::{DeferredEffect, EffectDispatch};

        let mut handler = self.clone();
        let table = cx.table().clone();
        let principal = cx.principal();
        let output = cx.user().clone();
        Ok(EffectDispatch::Deferred(DeferredEffect::blocking(
            move || {
                let owned = tidepool_effect::dispatch::EffectContext::with_principal(
                    &table, principal, &output,
                );
                handler.handle(req, &owned)
            },
        )))
    }

    fn handle(
        &mut self,
        req: WorktreeReq,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        let principal = cx.principal();
        if matches!(&req, WorktreeReq::WorktreeCurrentWorkspace) {
            let result = (|| {
                let (tree, access) = if let Some(tree) = self.authority.bound_worktree(principal) {
                    let access = self
                        .authority
                        .workspace_access(principal, &tree)
                        .ok_or_else(|| {
                            WorktreeError::WorktreeUnauthorized(worktree_id_to_wire(&tree))
                        })?;
                    (tree, access)
                } else if self.authority.has_repository_access(principal) {
                    let handle = self
                        .inner
                        .manager
                        .register_source_checkout()
                        .map_err(error_to_wire)?;
                    let access =
                        if self.authority.grant(principal) == ActorWorktreeGrant::Repository {
                            WorkspaceAccess::ReadWrite
                        } else {
                            WorkspaceAccess::ReadOnly
                        };
                    (handle.id().clone(), access)
                } else {
                    return Err(WorktreeError::WorktreeAuthorityDenied(
                        "currentWorkspace requires a live workspace attachment".into(),
                    ));
                };
                self.inner
                    .manager
                    .lookup(&tree)
                    .map_err(error_to_wire)?
                    .ok_or_else(|| never_registered(&worktree_id_to_wire(&tree)))?;
                self.authority
                    .issue_workspace(principal, &tree, access)
                    .map(|raw| tidepool_bridge_effects::WtWorkspaceHandle { raw })
                    .map_err(error_to_wire)
            })();
            return cx.respond(result);
        }
        let ActorWorktreeGrant::Bound {
            enumerate,
            allocate,
            integrate,
        } = self.authority.grant(principal)
        else {
            let grant = self.authority.grant(principal);
            if grant == ActorWorktreeGrant::RepositoryReadOnly {
                let denied = WorktreeError::WorktreeAuthorityDenied(
                    "this actor may inspect repository worktrees but may not modify them".into(),
                );
                match &req {
                    WorktreeReq::WorktreeCreate(..) => {
                        return cx.respond(Err::<WtWorktreeHandle, _>(denied));
                    }
                    WorktreeReq::WorktreeTryMerge(..) => {
                        return cx.respond(Err::<WtMergeOutcome, _>(denied));
                    }
                    _ => {}
                }
            }
            if matches!(&req, WorktreeReq::WorktreeBound) {
                if camino::Utf8Path::from_path(self.inner.manager.source_repository()).is_none() {
                    return cx.respond(Err::<(), _>(WorktreeError::WorktreeAuthorityDenied(
                        "the source checkout path is not valid UTF-8 and cannot cross the Haskell boundary"
                            .into(),
                    )));
                }
                return cx.respond(
                    self.inner
                        .manager
                        .register_source_checkout()
                        .map(|handle| handle_to_wire(&handle))
                        .map_err(error_to_wire),
                );
            }
            if let WorktreeReq::WorktreeCreate(spec) = &req {
                if grant == ActorWorktreeGrant::Repository {
                    let result = (|| {
                        let spec = spec_from_wire(spec.clone())?;
                        self.inner
                            .manager
                            .root_allocations()
                            .create(&spec)
                            .map(|handle| handle_to_wire(&handle))
                            .map_err(error_to_wire)
                    })();
                    return cx.respond(result);
                }
                return cx.respond(Err::<WtWorktreeHandle, _>(
                    WorktreeError::WorktreeAuthorityDenied(
                        "this actor may not allocate a project worktree".into(),
                    ),
                ));
            }
            return tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, req, cx);
        };
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
            WorktreeReq::WorktreeLookup(_)
            | WorktreeReq::WorktreeBranchOf(_)
            | WorktreeReq::WorktreeHeadOf(_)
            | WorktreeReq::WorktreeObserveSubmission(_) => {
                // A known checkout may be observed without transferring its
                // binding or conferring authority to mutate it.
                return tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, req, cx);
            }
            WorktreeReq::WorktreeCreate(spec) if allocate => {
                let result = (|| {
                    let spec = spec_from_wire(spec.clone())?;
                    self.authorize_source(principal, &spec.source)?;
                    self.inner
                        .manager
                        .create(&spec)
                        .map(|handle| handle_to_wire(&handle))
                        .map_err(error_to_wire)
                })();
                return cx.respond(result);
            }
            WorktreeReq::WorktreeList | WorktreeReq::WorktreeListMatching(..) if enumerate => {
                return tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, req, cx);
            }
            WorktreeReq::WorktreeTryMerge(request) if integrate => Some(&request.target_worktree),
            WorktreeReq::WorktreeCreate(_)
            | WorktreeReq::WorktreeCurrentWorkspace
            | WorktreeReq::WorktreeList
            | WorktreeReq::WorktreeListMatching(..)
            | WorktreeReq::WorktreeTryMerge(..) => None,
        };
        if let Some(wire_id) = permitted_tree {
            let id = match worktree_id_from_wire(wire_id) {
                Ok(id) => id,
                Err(error) => return cx.respond(Err::<(), _>(error)),
            };
            if self.authority.owns(principal, &id)
                && (!matches!(&req, WorktreeReq::WorktreeTryMerge(_))
                    || self.authority.workspace_access(principal, &id)
                        == Some(WorkspaceAccess::ReadWrite))
            {
                return tidepool_effect::dispatch::EffectHandler::handle(&mut self.inner, req, cx);
            }
            let operation = match &req {
                WorktreeReq::WorktreeLookup(_) => "worktreeLookup",
                WorktreeReq::WorktreeBranchOf(_) => "worktreeBranchOf",
                WorktreeReq::WorktreeHeadOf(_) => "worktreeHeadOf",
                WorktreeReq::WorktreeObserveSubmission(_) => "worktreeObserveSubmission",
                _ => "worktreeAccess",
            };
            tracing::warn!(operation, principal = ?principal, worktree = %id, "worktree operation denied");
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
// `exomonad_worktree`'s domain types of the same shape (the wire side carries
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

pub(crate) use crate::generated::worktree_adapters::{
    branch_name_from_wire, branch_name_to_wire, dirty_policy_from_wire, git_oid_to_wire,
    git_ref_from_wire, git_ref_to_wire, in_progress_kind_to_wire, worktree_id_to_wire,
};

/// The trust boundary where a wire id becomes a domain id.
///
/// The MECHANICAL half is generated: `WtWorktreeId::new` is the schema's
/// `Validation::Segment { max_len: 128, extra_allowed: "-_" }`, which is
/// byte-for-byte `exomonad_worktree::WorktreeId::is_path_safe` expressed as
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

pub fn worktree_source_from_wire(
    source: WtWorktreeSource,
) -> Result<WorktreeSource, WorktreeError> {
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
        branch: r.branch.as_ref().map(branch_name_to_wire),
        source_head: git_oid_to_wire(&r.source_head),
        snapshot_ref: r.snapshot_ref.as_ref().map(git_ref_to_wire),
        created_at: r.created_at_ms,
    }
}

pub fn handle_to_wire(h: &WorktreeHandle) -> WtWorktreeHandle {
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

/// Total map from the domain `WorktreeError` (`exomonad/worktree/src/error.rs`)
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
        DomainWorktreeError::GitRepositoryIdentityMismatch { path, detail } => {
            WorktreeError::GitRepositoryIdentityMismatch(
                path.to_string_lossy().into_owned(),
                detail,
            )
        }
        DomainWorktreeError::WorktreeLost(id) => {
            WorktreeError::WorktreeLost(worktree_id_to_wire(&id))
        }
        // Distinct from `WorktreeLost` on the wire, matching the domain's own
        // reasoning (exomonad/worktree/src/error.rs): collapsing "never
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

    pub(crate) fn worktree_current_workspace(
        &mut self,
    ) -> Result<tidepool_bridge_effects::WtWorkspaceHandle, WorktreeError> {
        Err(WorktreeError::WorktreeAuthorityDenied(
            "workspace capabilities require the actor authority interpreter".into(),
        ))
    }

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
    /// `exomonad/worktree/src/registry.rs`'s `WorktreeRegistry::get` doc.
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
                branch_prefix.as_deref().is_none_or(|prefix| {
                    summary
                        .receipt
                        .branch
                        .as_ref()
                        .is_some_and(|branch| branch.as_str().starts_with(prefix))
                })
            })
            .filter(|summary| {
                created_after.is_none_or(|timestamp| summary.receipt.created_at_ms > timestamp)
            })
            .map(summary_to_wire)
            .collect())
    }

    /// Reads the branch fresh from git rather than returning the receipt's
    /// recorded one — reconciled inspection is the only source of truth (see
    /// `exomonad/worktree/CLAUDE.md`).
    pub(crate) fn worktree_branch_of(
        &mut self,
        tree_id: WtWorktreeId,
    ) -> Result<WtBranchName, WorktreeError> {
        let id = worktree_id_from_wire(&tree_id)?;
        let branch = self
            .manager
            .worktree_branch_by_id(&id)
            .map_err(error_to_wire)?
            .ok_or_else(|| never_registered(&tree_id))?;
        Ok(branch_name_to_wire(&branch))
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
    /// A fresh manager read of Git administration on every call. A retained
    /// view needs no full working-file restoration for this observation.
    pub(crate) fn worktree_head_of(
        &mut self,
        tree_id: WtWorktreeId,
    ) -> Result<WtGitOid, WorktreeError> {
        let id = worktree_id_from_wire(&tree_id)?;
        let head = self
            .manager
            .worktree_head_by_id(&id)
            .map_err(error_to_wire)?
            .ok_or_else(|| never_registered(&tree_id))?;
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
    /// `exomonad/worktree/src/merge.rs`): merge `branch` into the worktree
    /// `target_worktree` names, using the explicitly named registered source
    /// checkout for nested kit object transfer, through
    /// `exomonad_worktree::merge::try_merge`
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
        let source_worktree_id = worktree_id_from_wire(&request.source_worktree)?;
        let source_handle = self
            .manager
            .lookup(&source_worktree_id)
            .map_err(error_to_wire)?
            .ok_or_else(|| never_registered(&request.source_worktree))?;
        let source = exomonad_worktree::GitOid::from_raw(request.source_head.raw);
        let source_branch = request.source_branch.as_ref().map(branch_name_from_wire);
        let advance = request.merge_advance.as_ref().map(branch_name_from_wire);
        let outcome = try_merge(
            self.manager.git(),
            handle.cwd(),
            source_handle.cwd(),
            &source,
            source_branch.as_ref(),
            advance.as_ref(),
            &request.merge_message,
        )
        .map_err(error_to_wire)?;
        Ok(merge_outcome_to_wire(outcome))
    }
}

/// What a worktree refusal says to whoever asked for the worktree.
///
/// The wire enum is generated and carries only doc comments, so a caller that
/// formats it with `Debug` hands the reader a Rust struct dump — which is what
/// the fork-a-child-into-a-worktree path did, on the busiest path in the
/// harness, discarding the remedy every one of these failures has. The
/// sentences match `Tidepool.Worktree.renderWorktreeError`, which says the same
/// things to the Haskell surface.
#[must_use]
pub fn render_worktree_error(error: &WorktreeError) -> String {
    match error {
        WorktreeError::SourceDirty(summary) => format!(
            "source repository is dirty: {} staged, {} unstaged, {} untracked; commit or stash \
             those changes, or call allowDirtySnapshot on the spec to snapshot the source as it \
             stands",
            summary.staged.len(),
            summary.unstaged.len(),
            summary.untracked.len()
        ),
        WorktreeError::NotARepository(path) => format!("not a git repository: {path}"),
        WorktreeError::GitRepositoryIdentityMismatch(path, detail) => {
            format!("Git repository identity mismatch at {path}: {detail}")
        }
        WorktreeError::WorktreeLost(id) => format!(
            "managed worktree {} is registered but missing on disk",
            id.raw
        ),
        WorktreeError::DirtySubmoduleUnsupported(path) => {
            format!("a dirty submodule is not supported: {path}")
        }
        WorktreeError::SourceOperationInProgress(kind) => format!(
            "source repository has an operation in progress: {kind:?}; finish or abort it first \
             — there is no snapshot override for this one, because a tree captured mid-operation \
             is not the tree anyone meant"
        ),
        WorktreeError::WorktreeBusy(id, holder) => format!(
            "worktree {} has an exclusive Git operation held by {holder}",
            id.raw
        ),
        WorktreeError::SubmissionUnstable(id) => format!(
            "worktree {} kept changing while its submission was observed; let the actor writing \
             in it settle, then observe again",
            id.raw
        ),
        WorktreeError::WorktreeUnauthorized(id) => format!(
            "worktree {} is observable, but this operation requires its binding or an explicit \
             integration grant; ask the owning actor to perform it",
            id.raw
        ),
        WorktreeError::WorktreeAuthorityDenied(detail) => {
            format!("worktree authority denied: {detail}")
        }
        WorktreeError::GitFailure(receipt) => format!(
            "git {} failed: {}",
            receipt.git_args.join(" "),
            receipt.git_stderr.trim()
        ),
        WorktreeError::WorktreeNotRegistered(id) => format!(
            "no worktree is registered with id {}; the id is stale or mistyped, and nothing was \
             lost",
            id.raw
        ),
        WorktreeError::InvalidRegistryRoot(root, detail) => {
            format!("registry root {root} must live outside every source repository: {detail}")
        }
        WorktreeError::StorageFailure(path, detail) => {
            format!("Tidepool's own storage failed at {path}: {detail}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The wire enums/newtypes these tables construct directly. The module body
    // no longer names them — its mechanical conversions are generated — but the
    // tables below still build wire values by hand, which is the point: they
    // assert against literals, not against the conversions under test.
    use exomonad_worktree::create::DirtyPolicy;
    use exomonad_worktree::error::InProgressKind;
    use exomonad_worktree::id::{GitOid, GitRef};
    use tidepool_bridge_effects::{WtDirtyPolicy, WtGitRef, WtInProgressKind, WtWorktreeSource};

    #[test]
    fn actor_worktree_authority_is_exact_to_resource_and_incarnation() {
        let storage = tempfile::tempdir().unwrap();
        let storage_anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
        let bindings = Arc::new(Mutex::new(BindingTable::open(&storage_anchor, "").unwrap()));
        let authority = ActorWorktreeAuthority::new("run-1", Arc::clone(&bindings));
        let root = tidepool_repr::PrincipalId::new(1, 1);
        let worker = tidepool_repr::PrincipalId::new(2, 3);
        let tree = WorktreeId::from_raw("worker-tree");
        authority.install_grant(root, ActorWorktreeGrant::Repository);

        let worker_principal = WorktreePrincipal::exact_actor("run-1", 2, 3);
        let binding = bindings
            .lock()
            .bind(&tree, &worker_principal, WorkspaceAccess::ReadWrite, 1)
            .unwrap();

        let peer = tidepool_repr::PrincipalId::new(4, 1);
        authority.install_grant(peer, ActorWorktreeGrant::Repository);
        assert!(authority.has_repository_access(root));
        assert!(authority.has_repository_access(peer));
        assert!(!authority.has_repository_access(tidepool_repr::PrincipalId::new(4, 2)));
        authority.remove_grant(root);
        assert!(!authority.has_repository_access(root));
        assert!(authority.has_repository_access(peer));
        assert!(authority.has_repository_access(tidepool_repr::PrincipalId::SYSTEM));
        assert!(authority.owns(worker, &tree));
        assert!(!authority.owns(tidepool_repr::PrincipalId::new(2, 4), &tree));
        assert!(!authority.owns(tidepool_repr::PrincipalId::new(3, 3), &tree));
        assert!(!authority.owns(worker, &WorktreeId::from_raw("another-tree")));

        binding.release(&mut bindings.lock()).unwrap();
        assert!(!authority.owns(worker, &tree));
    }

    #[test]
    fn committed_fork_resolves_seed_and_does_not_capture_shared_dirt() {
        let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "seed\n", "seed")
            .unwrap();
        let committed = repository.writer().head().unwrap();
        repository
            .writer()
            .write_file("README.md", "live dirt\n")
            .unwrap();
        let storage = tempfile::tempdir().unwrap();
        let anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
        let manager = WorktreeManager::new(
            GitCli::new(),
            WorktreeRegistry::open(&anchor, "registry").unwrap(),
            storage.path().join("worktrees"),
            repository.path(),
        );
        let bindings = Arc::new(Mutex::new(BindingTable::open(&anchor, "bindings").unwrap()));
        let authority = ActorWorktreeAuthority::new("run", bindings);
        let root = tidepool_repr::PrincipalId::new(1, 1);
        authority.install_grant(root, ActorWorktreeGrant::Repository);
        let handler = ActorWorktreeHandler::new(WorktreeHandler::from_manager(manager), authority);
        let fork = handler
            .authorize_committed_fork(root, WtWorktreeSource::SourceCurrentRepository)
            .unwrap()
            .materialize_committed()
            .unwrap();
        assert_eq!(fork.handle_receipt.source_head.raw, committed.as_str());
        assert_eq!(
            fork.handle_receipt.branch.as_ref().unwrap().raw,
            format!("exomonad/{}", fork.handle_receipt.tree_id.raw)
        );
        assert_eq!(
            std::fs::read_to_string(Path::new(&fork.handle_receipt.cwd).join("README.md")).unwrap(),
            "seed\n"
        );
        assert_eq!(
            std::fs::read_to_string(repository.path().join("README.md")).unwrap(),
            "live dirt\n"
        );
        assert!(handler
            .authorize_committed_fork(
                tidepool_repr::PrincipalId::new(9, 1),
                WtWorktreeSource::SourceCurrentRepository
            )
            .is_err());
    }

    #[test]
    fn shared_workspace_capability_survives_issuer_release_and_cannot_widen() {
        let storage = tempfile::tempdir().unwrap();
        let anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
        let bindings = Arc::new(Mutex::new(BindingTable::open(&anchor, "bindings").unwrap()));
        let authority = ActorWorktreeAuthority::new("run", bindings.clone());
        let issuer = tidepool_repr::PrincipalId::new(1, 1);
        let peer = tidepool_repr::PrincipalId::new(2, 1);
        let tree = WorktreeId::from_raw("shared");
        let issuer_lease = bindings
            .lock()
            .bind(
                &tree,
                &WorktreePrincipal::exact_actor("run", 1, 1),
                WorkspaceAccess::ReadOnly,
                1,
            )
            .unwrap();
        let peer_lease = bindings
            .lock()
            .bind(
                &tree,
                &WorktreePrincipal::exact_actor("run", 2, 1),
                WorkspaceAccess::ReadWrite,
                2,
            )
            .unwrap();
        let token = authority
            .issue_workspace(issuer, &tree, WorkspaceAccess::ReadOnly)
            .unwrap();
        assert!(authority
            .issue_workspace(issuer, &tree, WorkspaceAccess::ReadWrite)
            .is_err());
        assert!(authority
            .resolve_workspace(&token, WorkspaceAccess::ReadWrite)
            .is_err());
        assert!(authority
            .resolve_workspace(tree.as_str(), WorkspaceAccess::ReadOnly)
            .is_err());
        issuer_lease.release(&mut bindings.lock()).unwrap();
        assert!(!authority.owns(issuer, &tree));
        assert!(authority.owns(peer, &tree));
        assert_eq!(
            authority
                .resolve_workspace(&token, WorkspaceAccess::ReadOnly)
                .unwrap(),
            tree
        );
        let other_run = ActorWorktreeAuthority::new("other-run", bindings.clone());
        assert!(other_run
            .resolve_workspace(&token, WorkspaceAccess::ReadOnly)
            .is_err());
        peer_lease.release(&mut bindings.lock()).unwrap();
    }

    #[test]
    fn worktree_query_filters_in_the_registry_handler() {
        let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "seed\n", "seed")
            .unwrap();
        let storage = tempfile::tempdir().unwrap();
        let storage_anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
        let registry = WorktreeRegistry::open(&storage_anchor, "registry").unwrap();
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
                receipt.branch.as_ref().map(|branch| branch.raw.clone()),
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
        let storage = DirectoryAnchor::open_existing(registry_dir.path()).unwrap();
        // `0x80` alone is not a valid UTF-8 lead byte.
        let bad_name = OsString::from_vec(vec![b'w', b't', 0x80]);
        let bad_root = registry_dir.path().join(PathBuf::from(bad_name));

        match WorktreeHandler::new(
            &storage,
            "registry",
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
            branch: Some(BranchName::from_raw("tidepool/worktree/wt-1")),
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
                "git_repository_identity_mismatch",
                DomainWorktreeError::GitRepositoryIdentityMismatch {
                    path: PathBuf::from("/repo/.exomonad/workspace"),
                    detail: "child gitdir resolves to parent metadata".into(),
                },
                WorktreeError::GitRepositoryIdentityMismatch(
                    "/repo/.exomonad/workspace".into(),
                    "child gitdir resolves to parent metadata".into(),
                ),
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
    /// "-_" }`), and as `exomonad_worktree::WorktreeId::is_path_safe`. They
    /// cannot be unified — `exomonad-worktree` is a DOMAIN crate and must not
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

macro_rules! installed_worktree_support {
    ($handler:ty, $key:ident) => {
        impl tidepool_mcp::InstalledEffectSupport for $handler {
            fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey> {
                vec![exomonad_tool::ActorEffectKey::$key.into()]
            }
        }
    };
}
installed_worktree_support!(ActorBoundWorktreeHandler, BoundWorktree);
installed_worktree_support!(ActorWorktreeRegistryHandler, WorktreeRegistry);
installed_worktree_support!(ActorWorktreeAllocationHandler, WorktreeAllocation);
installed_worktree_support!(ActorWorktreeIntegrationHandler, WorktreeIntegration);

// This umbrella handler backs the narrow facades above. It grants no
// additional actor-authored family of its own.
impl tidepool_mcp::InstalledEffectSupport for ActorWorktreeHandler {
    fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        Vec::new()
    }
}
