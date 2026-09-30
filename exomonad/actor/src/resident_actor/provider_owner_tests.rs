use super::*;
use tidepool_runtime::session::{ModuleEnv, SessionLib};

type Forest = ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>;

fn forest() -> (Forest, tempfile::TempDir) {
    let root = tempfile::tempdir().expect("session root");
    let session = tidepool_runtime::session::fresh_session_id();
    let library = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
        .expect("ephemeral declaration owner");
    let machine = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(library),
    );
    let (forest, _deployments) = ResidentForest::new(
        ActorWorkbenchSource::new("", Vec::new()),
        session,
        machine,
        None,
        crate::Incarnation::FIRST,
    );
    (forest, root)
}

async fn path_workbench(forest: &Forest, policy: crate::ActorPersistencePolicy) -> LocalActorRef {
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .expect("actual root placement");
    let descriptor = ActorDescriptor::new("owner fixture", placement)
        .with_actor_path(crate::ActorPath::parse("root").expect("canonical root"))
        .with_persistence_policy(policy);
    let behavior = ResidentKernelBehavior::with_boot(
        descriptor,
        forest.environment.clone(),
        ResidentBoot::Workbench,
        Vec::new(),
    );
    crate::local_actor::spawn_local_actor_in_directory(
        None,
        behavior,
        forest.incarnation,
        forest.directory.clone(),
    )
    .await
    .expect("actual local actor start")
    .0
}

#[tokio::test]
async fn allocated_actor_path_does_not_upgrade_ephemeral_provider_admission() {
    let (forest, _root) = forest();
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Ephemeral).await;
    let admission = forest
        .authorize_provider_attachment(actor.identity())
        .expect("explicit ephemeral actor is ready");
    assert!(admission.owner.durable().is_none());
    assert_eq!(
        admission.placement(),
        forest
            .directory
            .session_context(actor.identity())
            .expect("admitted context")
            .placement
    );
    assert!(forest
        .bind_durable_root_public_owner(actor.identity())
        .await
        .is_err());
    forest
        .validate_provider_attachment(&admission)
        .expect("failed upgrade leaves exact ephemeral owner");
    forest.shutdown().await;
    assert!(forest.validate_provider_attachment(&admission).is_err());
}

#[tokio::test]
async fn pending_durable_actor_refuses_provider_and_cell_admission_after_failed_initialization() {
    let (forest, _root) = forest();
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Durable).await;
    assert!(forest
        .authorize_provider_attachment(actor.identity())
        .is_err());
    // The fixture intentionally has no configured durable run owner. Refusal
    // must preserve Pending rather than admitting an ephemeral replacement.
    assert!(forest
        .bind_durable_root_public_owner(actor.identity())
        .await
        .is_err());
    assert!(forest
        .authorize_provider_attachment(actor.identity())
        .is_err());
    let error = crate::ResidentInteractivePolicy::local(actor)
        .dispatch_boxed(exomonad_tool::ToolInvocation {
            context: None,
            name: crate::HASKELL_TOOL.into(),
            arguments: exomonad_tool::ToolArguments::Raw("undefined".into()),
        })
        .await
        .expect_err("pending actor cannot begin compiler work");
    assert!(matches!(error, crate::ResidentToolError::Invocation(
        KernelInvocationFailure::Rejected { detail, .. }
    ) if detail == "durable actor public surface is not initialized"));
    forest.shutdown().await;
}

#[tokio::test]
async fn provider_admission_is_bound_to_the_original_forest_owner() {
    let (forest_a, _root_a) = forest();
    let (forest_b, _root_b) = forest();
    let actor_a = forest_a
        .new_workbench("a".into(), crate::EffectiveRole::root())
        .await
        .expect("first workbench");
    let actor_b = forest_b
        .new_workbench("b".into(), crate::EffectiveRole::root())
        .await
        .expect("second workbench");
    let admission = forest_a
        .authorize_provider_attachment(actor_a.identity())
        .expect("actual first owner");
    forest_a
        .validate_provider_attachment(&admission)
        .expect("same owner");
    assert!(forest_b.validate_provider_attachment(&admission).is_err());
    let other = forest_b
        .authorize_provider_attachment(actor_b.identity())
        .expect("second owner");
    assert!(!Arc::ptr_eq(&admission.owner, &other.owner));
    forest_a.shutdown().await;
    forest_b.shutdown().await;
}
