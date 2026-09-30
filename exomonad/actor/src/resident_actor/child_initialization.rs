//! A committed fork owner selects one child's exact retained lexical root.

use std::sync::Arc;

use tidepool_runtime::session::{RuntimeLexicalScopeLease, WorkbenchForkBoundary};

use crate::{ActorDescriptor, ActorRef};

/// One non-cloneable release of an already admitted child. The original
/// allocation and committed group authority travel with the runtime-owned
/// lexical share; a scope identifier alone cannot release a resident child.
pub struct ForkChildRelease {
    child: ActorRef,
    admitted: ActorDescriptor,
    publication_boundary: Option<WorkbenchForkBoundary>,
    authority: Arc<crate::lineage::CommittedForkGroups>,
    lexical: Arc<RuntimeLexicalScopeLease>,
}

impl ForkChildRelease {
    pub(super) fn issue(
        child: ActorRef,
        admitted: ActorDescriptor,
        publication_boundary: Option<WorkbenchForkBoundary>,
        authority: Arc<crate::lineage::CommittedForkGroups>,
        lexical: Arc<RuntimeLexicalScopeLease>,
    ) -> Result<Self, crate::KernelBehaviorError> {
        if admitted.creator() != Some(authority.owner())
            || admitted.actor_path().is_none()
            || admitted
                .fork_group()
                .is_none_or(|group| !authority.permits_child(group, child))
        {
            return Err(crate::KernelBehaviorError {
                detail: "child release differs from its committed group allocation".into(),
            });
        }
        Ok(Self {
            child,
            admitted,
            publication_boundary,
            authority,
            lexical,
        })
    }

    pub(crate) fn child(&self) -> ActorRef {
        self.child
    }

    pub(super) fn matches(&self, actor: ActorRef, descriptor: &ActorDescriptor) -> bool {
        actor == self.child
            && descriptor.placement() == self.admitted.placement()
            && descriptor.actor_path() == self.admitted.actor_path()
            && descriptor.creator() == Some(self.authority.owner())
            && descriptor.fork_group() == self.admitted.fork_group()
            && descriptor.fork_boundary() == self.admitted.fork_boundary()
            && descriptor.context_parent() == self.admitted.context_parent()
            && descriptor.checkpoint_token() == self.admitted.checkpoint_token()
            && descriptor.persistence_policy() == self.admitted.persistence_policy()
            && descriptor.source_layer() == self.admitted.source_layer()
    }

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
        ForkChildRelease::issue(child, descriptor.clone(), None, authority, lexical).unwrap();
    (release, descriptor, session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActorId;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_repr::ActorPath;

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
    fn committed_receipt_cannot_issue_release_for_unrelated_child() {
        let actor = ActorRef::first(ActorId(7));
        let (release, descriptor, _session) = scheduler_fixture(actor);
        assert!(ForkChildRelease::issue(
            ActorRef::first(ActorId(8)),
            descriptor.clone(),
            None,
            Arc::clone(&release.authority),
            Arc::clone(&release.lexical),
        )
        .is_err());
        assert!(ForkChildRelease::issue(
            actor,
            descriptor,
            None,
            Arc::clone(&release.authority),
            Arc::clone(&release.lexical),
        )
        .is_ok());
    }

    #[test]
    fn release_owns_independent_runtime_scope_and_refuses_other_runtime_owner() {
        let actor = ActorRef::first(ActorId(7));
        let (release, descriptor, mut session) = scheduler_fixture(actor);
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
