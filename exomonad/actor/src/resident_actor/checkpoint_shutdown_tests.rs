//! Checkpoint retirement keeps the actor shutdown deadline and retry evidence.

use super::*;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::SessionId;
use tidepool_runtime::session::{ContextCheckpointBoundary, ModuleEnv, SessionLib};

#[tokio::test(start_paused = true)]
async fn nonempty_checkpoint_shutdown_retains_uncertainty_and_retry_authority() {
    let mut owner = invocation_work::tests::Fixture::start().await;
    let root = tempfile::tempdir().unwrap();
    let id = SessionId(0xE205);
    let library = SessionLib::open(id, root.path(), ModuleEnv::standalone_default()).unwrap();
    let mut session = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(library),
    );
    let actor_scope = session.mint_isolated_scope();
    let machines = Arc::new(ActorMachineRegistry::new());
    machines.insert_idle(id, Box::new(session));
    owner.environment.runner =
        ResidentActorRunner::new(machines.clone(), ActorWorkbenchSource::new("", Vec::new()));
    let descriptor = ActorDescriptor::new(
        "checkpoint-shutdown",
        ActorPlacement {
            session: id,
            lexical_scope: actor_scope,
            resource_scope: RealmId::fresh(),
        },
    );
    let mut behavior = ResidentKernelBehavior::with_boot(
        descriptor,
        owner.environment.clone(),
        ResidentBoot::Workbench,
        Vec::new(),
    );
    let actor = owner.actor.identity();
    let mut source = behavior.context(actor);
    source.placement.lexical_scope = ScopeId::ROOT;
    let registry = behavior.environment.actor_admissions.clone();
    let mut captures = Vec::new();
    for name in ["pending", "released"] {
        let (scope, retained) = behavior
            .environment
            .runner
            .capture_retained_context_scope(source.clone())
            .await
            .unwrap();
        let token = registry.capture_checkpoint_with_retained_scope(
            name.into(),
            actor,
            ActorCapabilities::default(),
            None,
            None,
            CheckpointSourceLayer::default(),
            id,
            scope,
            ContextCheckpointBoundary::external(
                "checkpoint-shutdown".into(),
                name.into(),
                "native-capture".into(),
            ),
            None,
            retained,
            ActorPersistencePolicy::Ephemeral,
        );
        captures.push((token, scope));
    }
    let (pending, pending_scope) = &captures[0];
    let (released, released_scope) = &captures[1];
    registry.settle_checkpoint(released, id, true).unwrap();
    assert_eq!(
        registry.release_checkpoint(released, id).unwrap(),
        Some(*released_scope)
    );
    let checkout = machines.checkout_run(id).unwrap();
    let terminal = ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "held checkpoint owner".into(),
        diagnostic: None,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let (hook, realm) = tokio::time::timeout(
        Duration::from_secs(2),
        behavior.shutdown_components(&owner.kernel, &terminal, deadline),
    )
    .await
    .expect("checkpoint checkout must share the shutdown deadline");
    assert_eq!(hook, CleanupComponentOutcome::Confirmed);
    assert!(matches!(realm, CleanupComponentOutcome::Unconfirmed(_)));
    assert!(matches!(
        registry.checkpoint(pending, id),
        Err(CheckpointRefusal::CaptureFailed)
    ));
    assert_eq!(
        registry.failed_checkpoint_scopes(actor),
        vec![(id, *pending_scope)]
    );
    assert_eq!(
        registry.pending_release_scopes(id),
        vec![(released.clone(), *released_scope)]
    );
    tokio::time::timeout(
        Duration::from_secs(1),
        behavior.stopped(&owner.kernel, &terminal),
    )
    .await
    .expect("stopped must not reopen an unbounded checkpoint checkout");
    checkout.restore_suspended(Vec::new());

    let (hook, realm) = behavior
        .shutdown_components(
            &owner.kernel,
            &terminal,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await;
    assert_eq!(hook, CleanupComponentOutcome::Confirmed);
    assert_eq!(realm, CleanupComponentOutcome::Confirmed);
    assert!(registry.pending_release_scopes(id).is_empty());
    assert_eq!(registry.release_checkpoint(released, id).unwrap(), None);
    let mut checkout = machines.checkout_run(id).unwrap();
    assert!(checkout.machine().compile_view_in(*pending_scope).is_none());
    assert!(checkout
        .machine()
        .compile_view_in(*released_scope)
        .is_none());
    assert!(checkout.machine().compile_view_in(ScopeId::ROOT).is_some());
    checkout.restore_suspended(Vec::new());
    drop(behavior);
    owner.finish().await;
}
