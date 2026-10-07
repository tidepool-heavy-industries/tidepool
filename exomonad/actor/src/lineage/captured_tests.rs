use super::*;
use crate::{ActorDescriptor, ActorId, ActorPlacement, ActorCapabilities, HostedCheckpointContext};
use tidepool_codegen::suspension::RealmId;

fn fixture(
    captured: bool,
) -> (
    ForkGroupRegistry,
    ForkGroupId,
    ActorRef,
    ActorRef,
    ActorDescriptor,
    CheckpointLease,
    WorkbenchForkBoundary,
) {
    let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
    let owner = ActorRef::first(ActorId(1));
    let child = ActorRef::first(ActorId(2));
    let boundary =
        WorkbenchForkBoundary::external("thread".into(), "request".into(), "pending-call".into());
    let attachment = if captured {
        HostedCheckpointAttachment::captured(Arc::new(()))
    } else {
        HostedCheckpointAttachment::new(Arc::new(()))
    };
    let token = groups.capture_checkpoint_with_host_attachment(
        "capture".into(),
        owner,
        ActorCapabilities::default(),
        None,
        None,
        crate::CheckpointSourceLayer::default(),
        SessionId(7),
        ScopeId(3),
        boundary.clone(),
        Some(attachment),
    );
    // A delivered token is usable while the provider operation remains Pending.
    groups
        .settle_checkpoint(&token, SessionId(7), true)
        .unwrap();
    let (group, paths) = groups
        .begin_at_boundary(
            owner,
            ActorPath::parse("captured/work").unwrap(),
            vec![ActorPathSegment::new("child").unwrap()],
            None,
            boundary.clone(),
        )
        .unwrap();
    let (lease, attachment) = groups
        .claim_with_checkpoint(
            group,
            owner,
            &paths[0].allocated,
            Some((&token, SessionId(7))),
        )
        .unwrap()
        .checkpoint
        .unwrap();
    groups.attach_child(group, owner, child).unwrap();
    let descriptor = ActorDescriptor::new(
        "child",
        ActorPlacement {
            session: SessionId(7),
            resource_scope: RealmId(2),
            lexical_scope: ScopeId(4),
        },
    )
    .with_actor_path(paths[0].allocated.clone())
    .with_fork_group(group)
    .with_context_parent(owner)
    .with_checkpoint_token(Some(token));
    groups
        .retain_checkpoint_admission(
            group,
            owner,
            child,
            &descriptor,
            &lease,
            attachment.as_ref(),
        )
        .unwrap();
    (groups, group, owner, child, descriptor, lease, boundary)
}

#[tokio::test]
async fn captured_group_uses_retained_admission_after_release_while_parent_is_pending() {
    let (groups, group, owner, child, descriptor, lease, boundary) = fixture(true);
    assert_eq!(
        groups
            .checkpoint_admission(group, owner, child)
            .unwrap()
            .unwrap()
            .context,
        HostedCheckpointContext::Captured
    );
    groups
        .release_checkpoint(descriptor.checkpoint_token().unwrap(), SessionId(7))
        .unwrap();
    assert!(matches!(
        groups.checkpoint(descriptor.checkpoint_token().unwrap(), SessionId(7)),
        Err(CheckpointRefusal::ReleasedCheckpoint)
    ));
    let gate = groups.gate(group, child).unwrap();
    groups.request_commit(group, owner).unwrap();
    gate.mark_ready().unwrap();
    let authority = groups
        .publish_captured_group(group, owner, Some(&boundary), &[(child, descriptor)])
        .unwrap();
    assert_eq!(authority.owner(), owner);
    assert_eq!(authority.groups(), &[group]);
    gate.wait_committed().await.unwrap();
    assert_eq!(gate.publication().unwrap(), ForkGroupPublication::Captured);
    gate.mark_failed().unwrap();
    assert_eq!(gate.publication().unwrap(), ForkGroupPublication::Captured);
    assert!(groups
        .abort_selected_unpublished(owner, &[group])
        .is_empty());
    assert!(matches!(
        groups.publish_groups(&[group], owner),
        Err(ForkGroupError::NotReady(_))
    ));
    // Runtime capture delivery was settled before release. This does not
    // publish the Deferred provider call or complete its tool operation.
    lease.wait_published().await.unwrap();
}

#[test]
fn captured_group_refuses_native_or_changed_admission_and_ready_rejection_aborts() {
    for supported in [false, true] {
        let (groups, group, owner, child, mut descriptor, _lease, boundary) = fixture(supported);
        if supported {
            descriptor.set_lexical_scope(ScopeId(99));
        }
        let phase = groups.request_commit(group, owner).unwrap();
        groups.gate(group, child).unwrap().mark_ready().unwrap();
        assert!(matches!(
            groups.publish_captured_group(group, owner, Some(&boundary), &[(child, descriptor)]),
            Err(ForkGroupError::CheckpointAdmissionMismatch { .. })
        ));
        assert_eq!(*phase.borrow(), ForkGroupPhase::Ready);
        assert!(matches!(
            groups.abort(group, owner),
            Err(ForkGroupError::AlreadyCommitted(_))
        ));
        assert_eq!(
            groups.abort_selected_unpublished(owner, &[group]),
            vec![child]
        );
        assert_eq!(*phase.borrow(), ForkGroupPhase::Aborted);
    }
}

#[test]
fn captured_group_selected_admission_requires_exact_scope_and_execution_boundary() {
    let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
    let owner = ActorRef::first(ActorId(1));
    let child = ActorRef::first(ActorId(2));
    let boundary =
        WorkbenchForkBoundary::external("thread".into(), "request".into(), "pending-call".into());
    let (group, paths) = groups
        .begin_at_boundary(
            owner,
            ActorPath::parse("selected/work").unwrap(),
            vec![ActorPathSegment::new("child").unwrap()],
            None,
            boundary.clone(),
        )
        .unwrap();
    groups.claim(group, owner, &paths[0].allocated).unwrap();
    groups.attach_child(group, owner, child).unwrap();
    let selected = ActorDescriptor::new(
        "selected",
        ActorPlacement {
            session: SessionId(8),
            resource_scope: RealmId(3),
            lexical_scope: ScopeId(1),
        },
    )
    .with_actor_path(paths[0].allocated.clone())
    .with_fork_group(group);
    groups
        .retain_selected_admission(group, owner, child, &selected)
        .unwrap();
    let phase = groups.request_commit(group, owner).unwrap();
    groups.gate(group, child).unwrap().mark_ready().unwrap();
    let other =
        WorkbenchForkBoundary::external("thread".into(), "request".into(), "other-call".into());
    assert!(matches!(
        groups.publish_captured_group(group, owner, Some(&other), &[(child, selected.clone())]),
        Err(ForkGroupError::NotReady(_))
    ));
    assert_eq!(*phase.borrow(), ForkGroupPhase::Ready);
    groups
        .publish_captured_group(group, owner, Some(&boundary), &[(child, selected)])
        .unwrap();
}
