//! A committed fork owner selects one child's exact retained lexical root.

use std::sync::Arc;

use tidepool_runtime::session::{RuntimeLexicalScopeLease, WorkbenchForkBoundary};

use crate::{ActorDescriptor, ActorRef};

pub(super) struct ChildInitializationFrame {
    pub context: crate::ActorSessionContext,
    pub boot: super::ResidentBoot,
    pub lexical: Arc<RuntimeLexicalScopeLease>,
    pub durable: Option<tidepool_runtime::session::RecoveryPublicOwner>,
    pub readiness: Option<Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>>,
    pub checkpoint: Option<(
        crate::CheckpointLease,
        Option<crate::HostedCheckpointAttachment>,
    )>,
}

/// A visible child surface whose directory durability is still unconfirmed.
/// The original boot is kept here until confirmation or acknowledged stop.
pub(super) struct PublishedChildInitialization {
    pub frame: ChildInitializationFrame,
    pub detail: String,
}

pub(super) enum ChildWorkspacePreparation {
    None,
    Retained(Arc<dyn crate::ForkWorkspaceCustody>),
    Prepared(crate::PreparedForkWorkspace),
    Bound {
        admission: crate::fork_workspace::SharedForkWorkspaceAdmission,
        worktree: String,
        role: crate::ActorRole,
    },
}

pub(super) struct PreparedChildWorkspace {
    pub frame: ChildInitializationFrame,
    pub result:
        Result<Option<Arc<dyn crate::ForkWorkspaceCustody>>, crate::ResidentActorWorkbenchError>,
}

pub(super) async fn prepare_workspace(
    frame: ChildInitializationFrame,
    preparation: ChildWorkspacePreparation,
) -> PreparedChildWorkspace {
    let actor = frame.context.actor;
    let result = match preparation {
        ChildWorkspacePreparation::None => Ok(None),
        ChildWorkspacePreparation::Retained(custody) => Ok(Some(custody)),
        ChildWorkspacePreparation::Prepared(prepared) => install_workspace(
            tidepool_runtime::spawn_blocking_in_span(move || prepared.install(actor)).await,
        ),
        ChildWorkspacePreparation::Bound {
            admission,
            worktree,
            role,
        } => install_workspace(
            tidepool_runtime::spawn_blocking_in_span(move || {
                admission.install_custody(actor, &worktree, role)
            })
            .await,
        ),
    };
    PreparedChildWorkspace { frame, result }
}

fn install_workspace(
    result: Result<
        Result<
            Arc<dyn crate::ForkWorkspaceCustody>,
            crate::fork_workspace::ForkWorkspaceAdmissionError,
        >,
        tokio::task::JoinError,
    >,
) -> Result<Option<Arc<dyn crate::ForkWorkspaceCustody>>, crate::ResidentActorWorkbenchError> {
    result
        .map_err(crate::ResidentActorWorkbenchError::Join)?
        .map(Some)
        .map_err(|error| crate::ResidentActorWorkbenchError::ActorProtocol(error.to_string()))
}

/// One child boot accepts one committed allocation and exact lexical grant.
/// Repeated delivery of that same custody cannot start another native boot.
#[derive(Default)]
pub(super) enum ForkChildReleaseState {
    #[default]
    Pending,
    Released {
        child: ActorRef,
        admitted: ActorDescriptor,
        publication_boundary: Option<WorkbenchForkBoundary>,
        authority: Arc<crate::lineage::CommittedForkGroups>,
        inherited: Option<crate::ActorSourceImports>,
    },
}

impl ForkChildReleaseState {
    pub(super) fn released(release: &ForkChildRelease) -> Self {
        Self::Released {
            child: release.child,
            admitted: release.admitted.clone(),
            publication_boundary: release.publication_boundary.clone(),
            authority: Arc::clone(&release.authority),
            inherited: release.inherited.clone(),
        }
    }

    pub(super) fn matches_replay(
        &self,
        release: &ForkChildRelease,
        actor: ActorRef,
        descriptor: &ActorDescriptor,
        lexical: Option<&Arc<RuntimeLexicalScopeLease>>,
    ) -> bool {
        let Self::Released {
            child,
            admitted,
            publication_boundary,
            authority,
            inherited,
        } = self
        else {
            return false;
        };
        let Some(lexical) = lexical else {
            return false;
        };
        *child == actor
            && release.child == actor
            && Arc::ptr_eq(authority, &release.authority)
            && publication_boundary == &release.publication_boundary
            && same_allocation(admitted, &release.admitted)
            && same_allocation(admitted, descriptor)
            && descriptor.placement().lexical_scope == lexical.scope()
            && Arc::ptr_eq(lexical, &release.lexical)
            && same_inherited_capture(inherited.as_ref(), release.inherited.as_ref())
            && match inherited {
                Some(expected) => {
                    same_inherited_capture(Some(expected), Some(descriptor.source_imports()))
                }
                None => descriptor.source_imports() == admitted.source_imports(),
            }
    }
}

fn same_inherited_capture(
    first: Option<&crate::ActorSourceImports>,
    second: Option<&crate::ActorSourceImports>,
) -> bool {
    match (first, second) {
        (None, None) => true,
        (Some(first), Some(second)) => match (first.inherited_scope(), second.inherited_scope()) {
            (Ok(Some(first)), Ok(Some(second))) => Arc::ptr_eq(&first, &second),
            _ => false,
        },
        _ => false,
    }
}

fn same_allocation(original: &ActorDescriptor, current: &ActorDescriptor) -> bool {
    original.placement().session == current.placement().session
        && original.placement().resource_scope == current.placement().resource_scope
        && original.actor_path() == current.actor_path()
        && original.creator() == current.creator()
        && original.supervisor_parent() == current.supervisor_parent()
        && original.fork_group() == current.fork_group()
        && original.fork_boundary() == current.fork_boundary()
        && original.context_parent() == current.context_parent()
        && original.checkpoint_token() == current.checkpoint_token()
        && original.profile() == current.profile()
        && original.capabilities() == current.capabilities()
        && original.persistence_policy() == current.persistence_policy()
        && original.source_layer() == current.source_layer()
}

/// One non-cloneable release of an already admitted child. The original
/// allocation and committed group authority travel with the runtime-owned
/// lexical share; a scope identifier alone cannot release a resident child.
pub struct ForkChildRelease {
    child: ActorRef,
    admitted: ActorDescriptor,
    publication_boundary: Option<WorkbenchForkBoundary>,
    authority: Arc<crate::lineage::CommittedForkGroups>,
    lexical: Arc<RuntimeLexicalScopeLease>,
    inherited: Option<crate::ActorSourceImports>,
}

impl ForkChildRelease {
    pub(super) fn issue(
        child: ActorRef,
        admitted: ActorDescriptor,
        publication_boundary: Option<WorkbenchForkBoundary>,
        authority: Arc<crate::lineage::CommittedForkGroups>,
        lexical: Arc<RuntimeLexicalScopeLease>,
        inherited: Option<Arc<RuntimeLexicalScopeLease>>,
    ) -> Result<Self, crate::KernelBehaviorError> {
        if admitted.creator() != Some(authority.owner())
            || admitted.actor_path().is_none()
            || admitted
                .fork_group()
                .is_none_or(|group| !authority.permits_child(group, child))
        {
            return Err(crate::KernelBehaviorError {
                detail: "child release differs from its committed group allocation".into(),
                diagnostic: None,
            });
        }
        if let Some(capture) = &inherited {
            if admitted.checkpoint_token().is_some()
                || admitted
                    .source_imports()
                    .inherited_scope()
                    .map_err(|error| crate::KernelBehaviorError {
                        detail: error.to_string(),
                        diagnostic: None,
                    })?
                    .is_none()
                || admitted.fork_boundary() != publication_boundary.as_ref()
                || publication_boundary.is_none()
                || capture.scope() == lexical.scope()
            {
                return Err(crate::KernelBehaviorError {
                    detail: "final inherited release differs from its original context admission"
                        .into(),
                    diagnostic: None,
                });
            }
        }
        Ok(Self {
            child,
            admitted,
            publication_boundary,
            authority,
            lexical,
            inherited: inherited.map(crate::ActorSourceImports::from_inherited_scope),
        })
    }

    pub(crate) fn child(&self) -> ActorRef {
        self.child
    }

    pub(super) fn matches(&self, actor: ActorRef, descriptor: &ActorDescriptor) -> bool {
        actor == self.child
            && descriptor.placement().lexical_scope == self.admitted.placement().lexical_scope
            && descriptor.creator() == Some(self.authority.owner())
            && same_allocation(&self.admitted, descriptor)
            && descriptor.source_imports() == self.admitted.source_imports()
    }

    pub(super) fn into_context(
        self,
    ) -> (
        Arc<RuntimeLexicalScopeLease>,
        Option<crate::ActorSourceImports>,
    ) {
        (self.lexical, self.inherited)
    }

    pub(super) fn inherited(&self) -> Option<&crate::ActorSourceImports> {
        self.inherited.as_ref()
    }

    #[cfg(test)]
    pub(super) fn into_lexical(self) -> Arc<RuntimeLexicalScopeLease> {
        self.lexical
    }

    pub(super) fn lexical(&self) -> &Arc<RuntimeLexicalScopeLease> {
        &self.lexical
    }
}

impl std::fmt::Debug for ForkChildRelease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ForkChildRelease")
            .field("child", &self.child)
            .field("admitted_placement", &self.admitted.placement())
            .field("publication_boundary", &self.publication_boundary)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
pub(crate) fn scheduler_fixture(
    child: ActorRef,
) -> (
    ForkChildRelease,
    ActorDescriptor,
    tidepool_runtime::session::PersistentSession,
) {
    use crate::ActorId;
    use crate::ActorPlacement;
    use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
    use tidepool_repr::{ActorPath, ActorPathSegment, SessionId};

    let owner = ActorRef::first(ActorId(u64::MAX));
    let groups = crate::ForkGroupRegistry::new(crate::ActorLineageRegistry::default());
    let (group, paths) = groups
        .begin(
            owner,
            ActorPath::parse("root/probe").unwrap(),
            vec![ActorPathSegment::new("child").unwrap()],
            None,
        )
        .unwrap();
    let path = groups.claim(group, owner, &paths[0].allocated).unwrap();
    groups.attach_child(group, owner, child).unwrap();
    groups.request_commit(group, owner).unwrap();
    groups.gate(group, child).unwrap().mark_ready().unwrap();
    let authority = Arc::new(groups.publish_groups(&[group], owner).unwrap());
    let mut session = tidepool_runtime::session::PersistentSession::new(None, 64 * 1024);
    let original = session.mint_scope(ScopeId::ROOT).unwrap();
    let lexical = session.retain_lexical_scope(original).unwrap();
    let descriptor = ActorDescriptor::new(
        "probe",
        ActorPlacement {
            session: SessionId(1),
            resource_scope: RealmId::ROOT,
            lexical_scope: original,
        },
    )
    .with_creator(owner)
    .with_actor_path(path)
    .with_fork_group(group);
    let release =
        ForkChildRelease::issue(child, descriptor.clone(), None, authority, lexical, None).unwrap();
    (release, descriptor, session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActorId;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_repr::ActorPath;

    fn preparation_frame() -> (
        ChildInitializationFrame,
        tidepool_runtime::session::PersistentSession,
    ) {
        let actor = ActorRef::first(ActorId(7));
        let (release, descriptor, session) = scheduler_fixture(actor);
        let context = descriptor
            .with_lexical_scope(release.lexical.scope())
            .session_context(actor);
        (
            ChildInitializationFrame {
                context,
                boot: super::super::ResidentBoot::Workbench,
                lexical: release.into_lexical(),
                durable: None,
                readiness: None,
                checkpoint: None,
            },
            session,
        )
    }

    fn workspace_handle() -> tidepool_bridge_effects::WtWorktreeHandle {
        use tidepool_bridge_effects::{WtBranchName, WtGitOid, WtWorktreeId, WtWorktreeReceipt};
        tidepool_bridge_effects::WtWorktreeHandle {
            handle_receipt: WtWorktreeReceipt {
                tree_id: WtWorktreeId {
                    raw: "prepared-child".into(),
                },
                cwd: "/fixture-child".into(),
                branch: WtBranchName {
                    raw: "child".into(),
                },
                source_head: WtGitOid {
                    raw: "0123456789012345678901234567890123456789".into(),
                },
                snapshot_ref: None,
                created_at: 0,
            },
        }
    }

    struct WorkspaceOwner(Option<tokio::sync::oneshot::Sender<()>>);

    impl crate::ForkWorkspaceCustody for WorkspaceOwner {
        fn actor_stopped(&self, _: &crate::ActorTerminal) {}
        fn process_may_exist(&self) {}
    }

    impl Drop for WorkspaceOwner {
        fn drop(&mut self) {
            if let Some(send) = self.0.take() {
                send.send(()).ok();
            }
        }
    }

    #[tokio::test]
    async fn owned_workspace_failure_keeps_original_boot_and_lexical_share() {
        let (frame, mut session) = preparation_frame();
        let actor = frame.context.actor;
        let scope = frame.lexical.scope();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let called = Arc::clone(&calls);
        let prepared = crate::PreparedForkWorkspace::new(workspace_handle(), move |actual| {
            assert_eq!(actual, actor);
            called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(crate::ForkWorkspaceAdmissionError {
                detail: "configured installer refused".into(),
            })
        });
        let result = prepare_workspace(frame, ChildWorkspacePreparation::Prepared(prepared)).await;
        assert!(
            matches!(result.result, Err(crate::ResidentActorWorkbenchError::ActorProtocol(ref detail)) if detail == "configured installer refused")
        );
        assert!(matches!(
            result.frame.boot,
            super::super::ResidentBoot::Workbench
        ));
        assert_eq!(result.frame.context.actor, actor);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(session.scope_tree().is_live(scope));
        drop(result);
        let _ = session.retain_lexical_scope(ScopeId::ROOT).unwrap();
        assert!(!session.scope_tree().is_live(scope));
    }

    #[tokio::test]
    async fn owned_workspace_dropped_prepared_result_releases_its_custody_and_scope() {
        let (frame, mut session) = preparation_frame();
        let scope = frame.lexical.scope();
        let (dropped, observe) = tokio::sync::oneshot::channel();
        let prepared = crate::PreparedForkWorkspace::new(workspace_handle(), move |_| {
            Ok(Arc::new(WorkspaceOwner(Some(dropped))))
        });
        let result = prepare_workspace(frame, ChildWorkspacePreparation::Prepared(prepared)).await;
        assert!(result.result.is_ok());
        assert!(session.scope_tree().is_live(scope));
        drop(result);
        observe.await.unwrap();
        let _ = session.retain_lexical_scope(ScopeId::ROOT).unwrap();
        assert!(!session.scope_tree().is_live(scope));
    }

    #[tokio::test]
    async fn owned_workspace_cancelled_wait_keeps_installer_and_actor_scope_owners() {
        let (frame, mut session) = preparation_frame();
        let kept_by_actor = Arc::clone(&frame.lexical);
        let scope = kept_by_actor.scope();
        let (entered, observe) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let wait = std::sync::Mutex::new(wait);
        let (dropped, observe_drop) = tokio::sync::oneshot::channel();
        let prepared = crate::PreparedForkWorkspace::new(workspace_handle(), move |_| {
            entered.send(()).unwrap();
            wait.lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap();
            Ok(Arc::new(WorkspaceOwner(Some(dropped))))
        });
        let task = tokio::spawn(prepare_workspace(
            frame,
            ChildWorkspacePreparation::Prepared(prepared),
        ));
        observe.await.unwrap();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert!(session.scope_tree().is_live(scope));
        release.send(()).unwrap();
        observe_drop.await.unwrap();
        // The behavior's existing lease, rather than the cancelled future,
        // keeps the child target live until acknowledged owner cleanup.
        drop(kept_by_actor);
        let _ = session.retain_lexical_scope(ScopeId::ROOT).unwrap();
        assert!(!session.scope_tree().is_live(scope));
    }

    #[test]
    fn release_refuses_actor_allocation_scope_source_and_policy_changes() {
        let actor = ActorRef::first(ActorId(7));
        let (release, descriptor, _session) = scheduler_fixture(actor);
        assert!(release.matches(actor, &descriptor));
        assert!(!release.matches(ActorRef::first(ActorId(8)), &descriptor));
        assert!(!release.matches(actor, &descriptor.clone().with_lexical_scope(ScopeId(999))));
        assert!(!release.matches(
            actor,
            &descriptor
                .clone()
                .with_actor_path(ActorPath::parse("root/other").unwrap())
        ));
        assert!(!release.matches(
            actor,
            &descriptor
                .clone()
                .with_source_layer(vec!["/unrelated".into()])
        ));
        assert!(!release.matches(
            actor,
            &descriptor
                .clone()
                .with_persistence_policy(crate::ActorPersistencePolicy::Durable)
        ));
        // A refusal does not consume the original admitted selection.
        assert!(release.matches(actor, &descriptor));
    }

    #[test]
    fn released_child_accepts_only_exact_original_authority_and_lexical_replay() {
        let actor = ActorRef::first(ActorId(7));
        let (release, descriptor, mut session) = scheduler_fixture(actor);
        let state = ForkChildReleaseState::released(&release);
        let adopted = descriptor
            .clone()
            .with_lexical_scope(release.lexical.scope());
        assert!(!ForkChildReleaseState::Pending.matches_replay(
            &release,
            actor,
            &adopted,
            Some(&release.lexical),
        ));
        assert!(state.matches_replay(&release, actor, &adopted, Some(&release.lexical)));
        let exact = ForkChildRelease::issue(
            actor,
            adopted.clone(),
            release.publication_boundary.clone(),
            Arc::clone(&release.authority),
            Arc::clone(&release.lexical),
            None,
        )
        .unwrap();
        assert!(state.matches_replay(&exact, actor, &adopted, Some(&release.lexical)));
        let changed_boundary = ForkChildRelease::issue(
            actor,
            descriptor.clone(),
            Some(WorkbenchForkBoundary::Route {
                actor_id: 1,
                incarnation: 1,
                watch_id: 1,
            }),
            Arc::clone(&release.authority),
            Arc::clone(&release.lexical),
            None,
        )
        .unwrap();
        assert!(!state.matches_replay(&changed_boundary, actor, &adopted, Some(&release.lexical)));
        assert!(!state.matches_replay(
            &release,
            actor,
            &adopted.clone().with_lexical_scope(ScopeId(999)),
            Some(&release.lexical),
        ));

        let (foreign, _, _foreign_session) = scheduler_fixture(actor);
        let changed_authority = ForkChildRelease::issue(
            actor,
            descriptor.clone(),
            release.publication_boundary.clone(),
            Arc::clone(&foreign.authority),
            Arc::clone(&release.lexical),
            None,
        )
        .unwrap();
        assert!(!state.matches_replay(&changed_authority, actor, &adopted, Some(&release.lexical)));
        assert!(!state.matches_replay(
            &release,
            ActorRef::first(ActorId(8)),
            &adopted,
            Some(&release.lexical)
        ));
        assert!(!state.matches_replay(
            &release,
            actor,
            &adopted.clone().with_source_layer(vec!["/changed".into()]),
            Some(&release.lexical),
        ));
        let alternate = session
            .retain_lexical_scope(descriptor.placement().lexical_scope)
            .unwrap();
        let changed_lexical = ForkChildRelease::issue(
            actor,
            descriptor,
            release.publication_boundary.clone(),
            Arc::clone(&release.authority),
            alternate,
            None,
        )
        .unwrap();
        assert!(!state.matches_replay(&changed_lexical, actor, &adopted, Some(&release.lexical)));
    }

    #[test]
    fn inherited_release_pairs_distinct_source_and_child_leases_and_replays_exact_capture() {
        let actor = ActorRef::first(ActorId(7));
        let (original, descriptor, mut session) = scheduler_fixture(actor);
        let boundary = WorkbenchForkBoundary::Execution {
            actor_id: descriptor.creator().unwrap().id.0,
            incarnation: descriptor.creator().unwrap().incarnation.0,
            execution_id: tidepool_runtime::session::workbench::WorkbenchExecutionId::from_digest(
                [3; 16],
            ),
        };
        let admission = session
            .retain_lexical_scope(descriptor.placement().lexical_scope)
            .unwrap();
        let descriptor = descriptor
            .with_fork_boundary(Some(boundary.clone()))
            .with_source_imports(crate::ActorSourceImports::from_inherited_scope(admission));
        let source = session.retain_lexical_scope(ScopeId::ROOT).unwrap();
        let lexical = session.retain_lexical_scope(source.scope()).unwrap();
        let issue = |descriptor: ActorDescriptor, lexical, source| {
            ForkChildRelease::issue(
                actor,
                descriptor,
                Some(boundary.clone()),
                Arc::clone(&original.authority),
                lexical,
                Some(source),
            )
        };
        let release = issue(
            descriptor.clone(),
            Arc::clone(&lexical),
            Arc::clone(&source),
        )
        .unwrap();
        assert!(release.matches(actor, &descriptor));
        assert!(!release.matches(
            actor,
            &descriptor.clone().with_source_imports(Default::default())
        ));
        assert_ne!(source.scope(), lexical.scope());
        let sibling_lexical = session.retain_lexical_scope(source.scope()).unwrap();
        let sibling = issue(
            descriptor.clone(),
            Arc::clone(&sibling_lexical),
            Arc::clone(&source),
        )
        .unwrap();
        assert_ne!(sibling.lexical.scope(), lexical.scope());
        assert!(same_inherited_capture(
            release.inherited(),
            sibling.inherited()
        ));
        // An explicit checkpoint remains its selected capture, even when the
        // original descriptor also carries an inherited source observation.
        assert!(issue(
            descriptor
                .clone()
                .with_checkpoint_token(Some("unadmitted-checkpoint".into())),
            Arc::clone(&lexical),
            Arc::clone(&source),
        )
        .is_err());
        assert!(issue(descriptor.clone(), Arc::clone(&source), Arc::clone(&source)).is_err());
        assert!(issue(
            descriptor.clone().with_source_imports(Default::default()),
            Arc::clone(&lexical),
            Arc::clone(&source),
        )
        .is_err());
        let state = ForkChildReleaseState::released(&release);
        let retry = issue(
            descriptor.clone(),
            Arc::clone(&lexical),
            Arc::clone(&source),
        )
        .unwrap();
        let (installed, imports) = release.into_context();
        let observed_admission = descriptor.source_imports().clone();
        descriptor.source_imports().release_capture();
        let installed_descriptor = descriptor
            .clone()
            .with_lexical_scope(installed.scope())
            .with_source_imports(imports.unwrap());
        assert!(observed_admission.inherited_scope().is_err());
        assert!(Arc::ptr_eq(
            &installed_descriptor
                .source_imports()
                .inherited_scope()
                .unwrap()
                .unwrap(),
            &source,
        ));
        assert!(state.matches_replay(&retry, actor, &installed_descriptor, Some(&installed)));
        let another = session.retain_lexical_scope(ScopeId::ROOT).unwrap();
        let changed = installed_descriptor
            .clone()
            .with_source_imports(crate::ActorSourceImports::from_inherited_scope(another));
        assert!(!state.matches_replay(&retry, actor, &changed, Some(&installed)));
    }

    #[test]
    fn committed_receipt_cannot_issue_release_for_unrelated_child() {
        let actor = ActorRef::first(ActorId(7));
        let (release, descriptor, _session) = scheduler_fixture(actor);
        assert!(ForkChildRelease::issue(
            ActorRef::first(ActorId(8)),
            descriptor.clone(),
            None,
            Arc::clone(&release.authority),
            Arc::clone(&release.lexical),
            None,
        )
        .is_err());
        assert!(ForkChildRelease::issue(
            actor,
            descriptor,
            None,
            Arc::clone(&release.authority),
            Arc::clone(&release.lexical),
            None,
        )
        .is_ok());
    }

    #[test]
    fn release_owns_independent_runtime_scope_and_refuses_other_runtime_owner() {
        let actor = ActorRef::first(ActorId(7));
        let (release, descriptor, mut session) = scheduler_fixture(actor);
        assert!(release.inherited().is_none());
        assert!(release.matches(actor, &descriptor));
        session.retire_scope(descriptor.placement().lexical_scope);
        assert!(!session
            .scope_tree()
            .is_live(descriptor.placement().lexical_scope));
        assert!(session.scope_tree().is_live(release.lexical.scope()));
        let mut foreign = tidepool_runtime::session::PersistentSession::new(None, 64 * 1024);
        assert!(foreign.mint_scope_from_lease(&release.lexical).is_err());
        let child = session.mint_scope_from_lease(&release.lexical).unwrap();
        assert!(session.scope_tree().is_live(child));
        session.retire_scope(child);
        let retained = release.lexical.scope();
        drop(release);
        // The existing runtime retirement queue is drained by ordinary entry.
        let _ = session.retain_lexical_scope(ScopeId::ROOT).unwrap();
        assert!(!session.scope_tree().is_live(retained));
    }
}
