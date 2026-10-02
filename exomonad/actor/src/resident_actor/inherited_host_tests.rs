use super::*;
use crate::{HostedCheckpointCapture, HostedCheckpointCaptureError};
use tidepool_runtime::session::WorkbenchForkBoundary;

struct Capture {
    calls: Mutex<Vec<(String, WorkbenchForkBoundary)>>,
    retained: Arc<String>,
    refuse: bool,
}

impl HostedCheckpointCapture for Capture {
    fn capture(
        &self,
        name: &str,
        boundary: &WorkbenchForkBoundary,
    ) -> Result<HostedCheckpointAttachment, HostedCheckpointCaptureError> {
        self.calls.lock().push((name.into(), boundary.clone()));
        if self.refuse {
            Err(HostedCheckpointCaptureError::CaptureFailed)
        } else {
            Ok(HostedCheckpointAttachment::captured(self.retained.clone()))
        }
    }
}

fn hosted() -> WorkbenchForkBoundary {
    WorkbenchForkBoundary::external("thread".into(), "response".into(), "call".into())
}

fn child() -> ActorDescriptor {
    ActorDescriptor::new(
        "group/child",
        crate::ActorPlacement {
            session: tidepool_repr::SessionId(8),
            resource_scope: tidepool_repr::RealmId(3),
            lexical_scope: tidepool_codegen::scope::ScopeId(1),
        },
    )
    .with_creator(ActorRef::first(crate::ActorId(1)))
    .with_context_parent(ActorRef::first(crate::ActorId(1)))
    .with_fork_group(crate::ForkGroupId(9))
    .with_fork_boundary(Some(hosted()))
}

fn publication(refuse: bool) -> (ForkPublication, Arc<Capture>) {
    let capture = Arc::new(Capture {
        calls: Mutex::new(Vec::new()),
        retained: Arc::new("durable provider cuts".into()),
        refuse,
    });
    (
        ForkPublication::Workbench {
            boundary: Some(hosted()),
            capture: Some(capture.clone()),
        },
        capture,
    )
}

#[test]
fn inherited_host_child_captures_exact_boundary_and_keeps_attachment_without_token() {
    let (publication, capture) = publication(false);
    let descriptor = child();
    assert!(descriptor.checkpoint_token().is_none());
    let admitted = publication
        .capture_inherited_child(&descriptor)
        .unwrap()
        .unwrap();
    assert_eq!(
        *capture.calls.lock(),
        vec![("group/child".into(), hosted())],
    );
    let retained = admitted.downcast::<String>().unwrap();
    assert!(Arc::ptr_eq(&retained, &capture.retained));
    // Child custody survives the issuing invocation's own capture owner.
    let installed = admitted.clone();
    drop(retained);
    drop(admitted);
    drop(publication);
    drop(capture);
    assert_eq!(
        &**installed.downcast::<String>().unwrap(),
        "durable provider cuts"
    );
}

#[test]
fn inherited_host_capture_leaves_explicit_checkpoint_and_selected_children_unchanged() {
    let (publication, capture) = publication(false);
    let explicit = child().with_checkpoint_token(Some("real-checkpoint-token".into()));
    assert!(publication
        .capture_inherited_child(&explicit)
        .unwrap()
        .is_none());
    let selected = ActorDescriptor::new("selected", child().placement())
        .with_fork_group(crate::ForkGroupId(9));
    assert!(publication
        .capture_inherited_child(&selected)
        .unwrap()
        .is_none());
    let fresh = ActorDescriptor::new("fresh", child().placement());
    assert!(publication
        .capture_inherited_child(&fresh)
        .unwrap()
        .is_none());
    assert!(capture.calls.lock().is_empty());
}

#[test]
fn inherited_host_capture_refuses_changed_boundary_and_capture_failure() {
    let (publication, capture) = publication(false);
    let other = child().with_fork_boundary(Some(WorkbenchForkBoundary::external(
        "thread".into(),
        "response".into(),
        "other-call".into(),
    )));
    assert!(publication.capture_inherited_child(&other).is_err());
    assert!(capture.calls.lock().is_empty());
    let (publication, capture) = self::publication(true);
    let error = publication.capture_inherited_child(&child()).unwrap_err();
    assert!(error.to_string().contains("CaptureFailed"));
    assert_eq!(
        *capture.calls.lock(),
        vec![("group/child".into(), hosted())]
    );
}

#[test]
fn inherited_host_capture_does_not_treat_direct_or_route_boundaries_as_provider_calls() {
    let (_, capture) = publication(false);
    for boundary in [
        WorkbenchForkBoundary::Route {
            actor_id: 1,
            incarnation: 1,
            watch_id: 9,
        },
        WorkbenchForkBoundary::Execution {
            actor_id: 1,
            incarnation: 1,
            execution_id: tidepool_runtime::session::WorkbenchExecutionId::from_digest([7; 16]),
        },
    ] {
        let publication = ForkPublication::Workbench {
            boundary: Some(boundary.clone()),
            capture: Some(capture.clone()),
        };
        let descriptor = child().with_fork_boundary(Some(boundary));
        assert!(publication
            .capture_inherited_child(&descriptor)
            .unwrap()
            .is_none());
    }
    assert!(capture.calls.lock().is_empty());
}
