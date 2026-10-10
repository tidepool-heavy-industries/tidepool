use super::*;
use std::path::PathBuf;
use tidepool_runtime::session::{ModuleEnv, SessionLib};

type Forest = ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>;

#[tokio::test]
async fn retirement_releases_inherited_capture_while_metadata_remains() {
    let (forest, _root) = forest();
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Ephemeral).await;
    let mut session = tidepool_runtime::session::PersistentSession::new(None, 1024);
    let source = session.mint_isolated_scope();
    let capture = session.retain_lexical_scope(source).unwrap();
    let captured_scope = capture.scope();
    let child = session.mint_scope_from_lease(&capture).unwrap();
    let child_lease = session.retain_lexical_scope(child).unwrap();
    let imports = crate::ActorSourceImports::from_inherited_scope(capture);
    let active_reader = imports.inherited_scope().unwrap().unwrap();
    let mut context_observation = forest.directory.session_context(actor.identity()).unwrap();
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
    let (forest_a, _root_a) = forest();
    let (forest_b, _root_b) = forest();
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

#[tokio::test]
async fn unscoped_program_child_of_durable_actor_cannot_gain_provider_attachment() {
    let (forest, _root) = forest();
    let parent = path_workbench(&forest, crate::ActorPersistencePolicy::Durable).await;
    let child = workbench_with_parent(
        &forest,
        crate::ActorPersistencePolicy::Ephemeral,
        Some(parent.identity()),
    )
    .await;
    assert!(forest
        .authorize_provider_attachment(child.identity())
        .is_err());
    // The program's own ephemeral plane remains valid for provider-free work;
    // attachment alone must not fabricate a durable path or inherited owner.
    let records = forest.environment.actors.lock();
    assert!(records[&child.identity()]
        .public_owner
        .ready()
        .unwrap()
        .durable()
        .is_none());
    drop(records);
    forest.shutdown().await;
}

#[tokio::test]
async fn requested_retirement_refuses_provider_before_terminal_publication() {
    let (forest, _root) = forest();
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Ephemeral).await;
    let admission = forest
        .authorize_provider_attachment(actor.identity())
        .expect("live owner");
    let requested = ActorTerminal {
        kind: crate::ActorExitKind::Completed,
        summary: "retirement during readiness".into(),
        diagnostic: None,
    };
    actor.terminal().request_shutdown(requested.clone());
    assert!(
        actor.terminal().get().is_none(),
        "intent precedes terminal cleanup"
    );
    assert!(forest.validate_provider_attachment(&admission).is_err());
    assert!(forest
        .authorize_provider_attachment(actor.identity())
        .is_err());
    assert_eq!(actor.terminal().requested_shutdown(), Some(requested));
    forest.shutdown().await;
}

#[tokio::test]
async fn native_manifest_uncertainty_fences_provider_and_sibling_owners_until_confirmation() {
    use std::os::unix::fs::PermissionsExt;
    use tidepool_runtime::session::{
        PublicManifestCommit, PublicationDecision, RecoveryPublicOwner,
    };
    struct RunOwner {
        root: PathBuf,
        _lock: std::fs::File,
    }
    impl tidepool_runtime::session::RecoveryRunAuthority for RunOwner {
        fn owns_run(&self, root: &std::path::Path) -> std::io::Result<bool> {
            Ok(root.canonicalize()? == self.root)
        }
    }
    let root = tempfile::tempdir().unwrap();
    let session = tidepool_runtime::session::fresh_session_id();
    let manifest = root.path().join("declarations.json");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.path().join("run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let mut library =
        SessionLib::open(session, root.path(), ModuleEnv::standalone_default()).unwrap();
    tidepool_testing::with_settlement(|settlement| {
        library.attach_owned_recovery_graph_v3(
            &manifest,
            Arc::new(RunOwner {
                root: root.path().canonicalize().unwrap(),
                _lock: lock,
            }),
            settlement,
        )
    })
    .unwrap();
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
    let actor = path_workbench(&forest, crate::ActorPersistencePolicy::Durable).await;
    assert_eq!(
        forest
            .bind_durable_root_public_owner(actor.identity())
            .await
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let provider = forest
        .authorize_provider_attachment(actor.identity())
        .unwrap();
    let context = forest.directory.session_context(actor.identity()).unwrap();
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
    assert!(sibling.is_ready());
    assert!(resident
        .durable_public_readiness(&owner, sibling_scope)
        .is_err());
    assert!(resident
        .confirm_durable_public_scope(&owner, sibling_scope)
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
    // Real native rename remains permitted; only parent-directory fsync fails.
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
    let commit = resident
        .publish_staged_public_manifest(ticket, &PublicationDecision::new())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        commit,
        PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
    ));
    assert!(!provider.owner.is_ready());
    assert!(!sibling.is_ready());
    assert!(initial_capability.is_current());
    let delayed_initial_owner =
        WorkbenchPublicOwner::issue(&context, &descriptor, Some(initial_capability))
            .expect("sibling uncertainty blocks readiness without revoking initial placement");
    assert!(!delayed_initial_owner.is_ready());
    assert!(matches!(
        resident.begin_durable_private_execution(&sibling_owner, sibling_scope),
        Err(
            tidepool_runtime::session::SessionError::InvalidDurablePublicAdmission {
                reason: tidepool_runtime::session::DurablePublicAdmissionFailure::Unconfirmed,
                ..
            }
        )
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
        .is_err());
    assert!(forest.validate_provider_attachment(&provider).is_err());
    let visible_bytes = std::fs::read(&manifest).unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
    assert!(forest
        .confirm_durable_root_public_owner(actor.identity())
        .await
        .is_err());
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!provider.owner.is_ready());
    assert!(!sibling.is_ready());
    assert!(forest.validate_provider_attachment(&provider).is_err());
    assert_eq!(
        forest
            .confirm_durable_root_public_owner(actor.identity())
            .await
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert!(sibling.is_ready());
    assert!(delayed_initial_owner.is_ready());
    forest
        .validate_provider_attachment(&provider)
        .expect("confirmation preserves exact provider owner Arc");
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        visible_bytes,
        "confirmation never republishes"
    );
    forest.shutdown().await;
    drop(actor);
    drop(forest);
    drop(machines);
    assert!(
        !sibling.is_ready(),
        "native graph destruction revokes retained readiness"
    );
    assert!(!delayed_initial_owner.is_current());
}
