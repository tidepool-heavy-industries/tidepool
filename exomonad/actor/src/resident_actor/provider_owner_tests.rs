use super::*;
use tidepool_runtime::session::{ModuleEnv, SessionLib};

type Forest = ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>;

#[tokio::test]
async fn retirement_releases_inherited_capture_while_metadata_remains() {
    let (forest, _root) = forest().await;
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Ephemeral).await;
    let mut session = tidepool_runtime::session::PersistentSession::new(None, 1024);
    let source = session.mint_isolated_scope();
    let capture = session.retain_lexical_scope(source).unwrap();
    let captured_scope = capture.scope();
    let child = session.mint_scope_from_lease(&capture).unwrap();
    let child_lease = session.retain_lexical_scope(child).unwrap();
    let imports = crate::ActorSourceImports::from_inherited_scope(capture);
    let active_reader = imports.inherited_scope().unwrap().unwrap();
    let mut context_observation = forest
        .directory
        .session_context(actor.identity())
        .await
        .unwrap()
        .unwrap();
    context_observation.source_imports = imports.clone();
    let descriptor_observation = {
        let mut records = forest.environment.actors.lock();
        let record = records.get_mut(&actor.identity()).unwrap();
        record.descriptor = record.descriptor.clone().with_source_imports(imports);
        record.descriptor.clone()
    };
    let terminal = ActorTerminal {
        kind: crate::ActorExitKind::Completed,
        summary: "capture retirement".into(),
        diagnostic: None,
    };
    retain_retired_metadata(&forest.environment, actor.identity(), &terminal);
    assert!(matches!(
        descriptor_observation.source_imports().inherited_scope(),
        Err(crate::ActorCompileViewError::ReleasedInheritedScope)
    ));
    assert!(matches!(
        context_observation.source_imports.inherited_scope(),
        Err(crate::ActorCompileViewError::ReleasedInheritedScope)
    ));
    assert!(session.scope_tree().is_live(captured_scope));
    drop(active_reader);
    let next_child = session.mint_scope_from_lease(&child_lease).unwrap();
    assert!(!session.scope_tree().is_live(captured_scope));
    assert!(session.scope_tree().is_live(next_child));
    assert_eq!(
        forest.environment.actors.lock()[&actor.identity()].terminal,
        Some(terminal)
    );
    session.retire_scope(child_lease.scope());
    drop(child_lease);
    session.retire_scope(source);
    session.retire_scope(child);
    session.retire_scope(next_child);
    assert_eq!(session.scope_tree().len(), 1);
    forest.shutdown().await;
}

async fn forest() -> (Forest, tempfile::TempDir) {
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
    )
    .await
    .unwrap();
    (forest, root)
}

async fn path_workbench(forest: &Forest, policy: crate::ActorPersistencePolicy) -> LocalActorRef {
    workbench_with_parent(forest, policy, None).await
}

async fn workbench_with_parent(
    forest: &Forest,
    policy: crate::ActorPersistencePolicy,
    parent: Option<ActorRef>,
) -> LocalActorRef {
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .expect("actual root placement");
    let mut descriptor = ActorDescriptor::new("owner fixture", placement)
        .with_actor_path(crate::ActorPath::parse("root").expect("canonical root"))
        .with_persistence_policy(policy);
    if let Some(parent) = parent {
        descriptor = descriptor.with_creator(parent);
    }
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
    let (forest, _root) = forest().await;
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Ephemeral).await;
    let admission = forest
        .authorize_provider_attachment(actor.identity())
        .await
        .expect("explicit ephemeral actor is ready");
    assert!(admission.owner.durable().is_none());
    assert_eq!(
        admission.placement(),
        forest
            .directory
            .session_context(actor.identity())
            .await
            .expect("directory observation")
            .expect("admitted context")
            .placement
    );
    assert!(forest
        .bind_durable_root_public_owner(actor.identity())
        .await
        .is_err());
    forest
        .validate_provider_attachment(&admission)
        .await
        .expect("failed upgrade leaves exact ephemeral owner");
    forest.shutdown().await;
    assert!(forest
        .validate_provider_attachment(&admission)
        .await
        .is_err());
}

#[tokio::test]
async fn pending_durable_actor_refuses_provider_and_cell_admission_after_failed_initialization() {
    let (forest, _root) = forest().await;
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Durable).await;
    assert!(forest
        .authorize_provider_attachment(actor.identity())
        .await
        .is_err());
    // The fixture intentionally has no configured durable run owner. Refusal
    // must preserve Pending rather than admitting an ephemeral replacement.
    assert!(forest
        .bind_durable_root_public_owner(actor.identity())
        .await
        .is_err());
    assert!(forest
        .authorize_provider_attachment(actor.identity())
        .await
        .is_err());
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        crate::resident_tools::ResidentToolClient::local(actor).dispatch_workbench(
            tidepool_runtime::session::WorkbenchRequest::from_cell_input("undefined"),
            None,
        ),
    )
    .await
    .expect("pending durable admission refuses without compiler work")
    .expect_err("pending actor cannot begin compiler work");
    assert!(matches!(error, crate::ResidentToolError::Invocation(
        KernelInvocationFailure::Rejected { detail, .. }
    ) if detail == "durable actor public surface is not initialized"));
    forest.shutdown().await;
}

#[tokio::test]
async fn provider_admission_is_bound_to_the_original_forest_owner() {
    let (forest_a, _root_a) = forest().await;
    let (forest_b, _root_b) = forest().await;
    let actor_a = forest_a
        .new_workbench("a".into(), crate::ActorCapabilities::default())
        .await
        .expect("first workbench");
    let actor_b = forest_b
        .new_workbench("b".into(), crate::ActorCapabilities::default())
        .await
        .expect("second workbench");
    let admission = forest_a
        .authorize_provider_attachment(actor_a.identity())
        .await
        .expect("actual first owner");
    forest_a
        .validate_provider_attachment(&admission)
        .await
        .expect("same owner");
    assert!(forest_b
        .validate_provider_attachment(&admission)
        .await
        .is_err());
    let other = forest_b
        .authorize_provider_attachment(actor_b.identity())
        .await
        .expect("second owner");
    assert!(!Arc::ptr_eq(&admission.owner, &other.owner));
    forest_a.shutdown().await;
    forest_b.shutdown().await;
}

#[tokio::test]
async fn unscoped_program_child_of_durable_actor_cannot_gain_provider_attachment() {
    let (forest, _root) = forest().await;
    let parent = path_workbench(&forest, crate::ActorPersistencePolicy::Durable).await;
    let child = workbench_with_parent(
        &forest,
        crate::ActorPersistencePolicy::Ephemeral,
        Some(parent.identity()),
    )
    .await;
    assert!(forest
        .authorize_provider_attachment(child.identity())
        .await
        .is_err());
    // The program's own ephemeral plane remains valid for provider-free work;
    // attachment alone must not fabricate a durable path or inherited owner.
    let owner = child.lifecycle().public_owner(None, false).await.unwrap();
    assert!(owner.durable().is_none());
    forest.shutdown().await;
}

#[tokio::test]
async fn requested_retirement_refuses_provider_before_terminal_publication() {
    let (forest, _root) = forest().await;
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Ephemeral).await;
    let admission = forest
        .authorize_provider_attachment(actor.identity())
        .await
        .expect("live owner");
    let requested = ActorTerminal {
        kind: crate::ActorExitKind::Completed,
        summary: "retirement during readiness".into(),
        diagnostic: None,
    };
    actor.terminal().request_shutdown(requested.clone());
    actor.lifecycle().close().await.unwrap();
    assert!(
        actor.terminal().get().is_none(),
        "intent precedes terminal cleanup"
    );
    assert!(forest
        .validate_provider_attachment(&admission)
        .await
        .is_err());
    assert!(forest
        .authorize_provider_attachment(actor.identity())
        .await
        .is_err());
    assert_eq!(actor.terminal().requested_shutdown(), Some(requested));
    forest.shutdown().await;
}

#[tokio::test]
async fn commit_error_fences_provider_and_sibling_owners_until_authenticated_reopen() {
    use tidepool_runtime::session::{
        PublicManifestCommit, PublicationDecision, RecoveryPublicOwner,
    };
    let root = tempfile::tempdir().unwrap();
    let session = tidepool_runtime::session::fresh_session_id();
    let recovery = crate::recovery::recovery_test_fixture::RecoveryFixture::create(root.path());
    let mut library = SessionLib::open(
        session,
        root.path().join("compiler"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut library, settlement));
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
    )
    .await
    .unwrap();
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Durable).await;
    let confirmed = forest
        .bind_durable_root_public_owner(actor.identity())
        .await
        .unwrap();
    assert!(matches!(confirmed, PublicManifestCommit::Durable));
    let provider = forest
        .authorize_provider_attachment(actor.identity())
        .await
        .unwrap();
    let provider_entries = std::sync::atomic::AtomicUsize::new(0);
    let admission = actor.lifecycle().transaction().await.unwrap();
    provider_entries.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    drop(admission);
    let context = forest
        .directory
        .session_context(actor.identity())
        .await
        .unwrap()
        .unwrap();
    let owner = provider.owner.durable().unwrap().clone();
    let machines = forest.environment.runner.machines_for_test().clone();
    let (mut resident, receipt) = machines.checkout_run(session).unwrap().into_parts();
    let sibling_scope = resident
        .mint_scope(context.placement.lexical_scope)
        .unwrap();
    let sibling_owner =
        RecoveryPublicOwner::new(&crate::ActorPath::parse("root/sibling").unwrap(), 1).unwrap();
    resident
        .initialize_durable_public_scope(sibling_owner.clone(), sibling_scope)
        .unwrap();
    let sibling = resident
        .durable_public_readiness(&sibling_owner, sibling_scope)
        .unwrap();
    assert!(sibling.check_ready_async().await.is_ok());
    assert!(resident
        .durable_public_readiness(&owner, sibling_scope)
        .is_err());
    let initial_capability = resident
        .durable_public_readiness(&owner, context.placement.lexical_scope)
        .unwrap();
    let descriptor = forest
        .environment
        .actors
        .lock()
        .get(&actor.identity())
        .unwrap()
        .descriptor
        .clone();
    let private = resident
        .begin_durable_private_execution(&owner, context.placement.lexical_scope)
        .unwrap();
    let intent = resident.freeze_private_execution(&private).unwrap();
    let tidepool_runtime::session::ExecutionPublication::Bindings(base) = resident
        .restage_execution_publication(owner.clone(), intent)
        .unwrap()
    else {
        panic!("empty private execution has a binding-only native publication");
    };
    let ticket = base.stage().unwrap();
    let original = recovery.public_owner(&owner).unwrap();
    recovery.fail_commits();
    let commit = resident
        .attempt_public_manifest(ticket, &PublicationDecision::new())
        .map(crate::resident_workbench::settled_manifest_attempt)
        .unwrap();
    recovery.clear_commit_failure();
    assert!(matches!(
        commit,
        PublicManifestCommit::RecoveryRequired { .. }
    ));
    assert!(!provider
        .native_readiness()
        .unwrap()
        .check_ready_async()
        .await
        .is_ok());
    assert!(actor.lifecycle().transaction().await.is_err());
    assert_eq!(
        provider_entries.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert!(!sibling.check_ready_async().await.is_ok());
    assert!(initial_capability.is_current());
    let delayed_initial_owner =
        WorkbenchPublicOwner::issue(&context, &descriptor, Some(initial_capability))
            .expect("sibling uncertainty blocks readiness without revoking initial placement");
    assert!(!delayed_initial_owner
        .native_readiness()
        .unwrap()
        .check_ready_async()
        .await
        .is_ok());
    assert!(matches!(
        resident.begin_durable_private_execution(&sibling_owner, sibling_scope),
        Err(tidepool_runtime::session::SessionError::RecoveryStore(_))
    ));
    drop(private);
    let holes = resident
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    machines.settle_suspended(receipt, resident, holes);
    assert!(forest
        .authorize_provider_attachment(actor.identity())
        .await
        .is_err());
    assert!(forest
        .validate_provider_attachment(&provider)
        .await
        .is_err());
    assert_eq!(recovery.public_owner(&owner).unwrap(), original);
    assert!(!provider
        .native_readiness()
        .unwrap()
        .check_ready_async()
        .await
        .is_ok());
    assert!(!sibling.check_ready_async().await.is_ok());
    assert!(forest
        .validate_provider_attachment(&provider)
        .await
        .is_err());
    let reopened = recovery.reopen();
    assert!(!reopened.store.is_recovery_required());
    assert!(recovery.store.is_recovery_required());
    let mut library = SessionLib::open(
        tidepool_runtime::session::fresh_session_id(),
        root.path().join("compiler"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    tidepool_testing::with_settlement(|settlement| reopened.attach_root(&mut library, settlement));
    let mut restored = tidepool_runtime::session::PersistentSession::new(Some(library), 1024);
    let scope = restored.recover_public_scope(&owner).unwrap();
    assert!(restored
        .durable_public_readiness(&owner, scope)
        .unwrap()
        .check_ready_async()
        .await
        .is_ok());
    assert_eq!(reopened.public_owner(&owner).unwrap(), original);
    assert!(!sibling.check_ready_async().await.is_ok());
    assert!(!delayed_initial_owner
        .native_readiness()
        .unwrap()
        .check_ready_async()
        .await
        .is_ok());
    assert!(forest
        .validate_provider_attachment(&provider)
        .await
        .is_err());
    assert!(
        matches!(confirmed, PublicManifestCommit::Durable),
        "later sibling fencing preserves the confirmed disposition"
    );
    assert!(actor.lifecycle().transaction().await.is_err());
    assert_eq!(
        provider_entries.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    forest.shutdown().await;
    drop(actor);
    drop(forest);
    drop(machines);
    assert!(
        !sibling.check_ready_async().await.is_ok(),
        "native graph destruction revokes retained readiness"
    );
    assert!(!delayed_initial_owner.is_current());
}
