use super::*;
use std::path::PathBuf;
use tidepool_codegen::scope::ScopeId;
use tidepool_runtime::session::ResidentError;

#[tokio::test]
async fn cancellation_during_original_preview_refuses_initial_provider_installation() {
    interrupt_during_preview(false, Interruption::Cancellation, true, false).await;
}

#[tokio::test]
async fn cancellation_during_original_preview_refuses_later_request_activation() {
    interrupt_during_preview(true, Interruption::Cancellation, false, false).await;
}

#[tokio::test]
async fn terminal_request_during_original_preview_retires_unpublished_input() {
    interrupt_during_preview(false, Interruption::TerminalRequest, false, false).await;
}

#[tokio::test]
async fn actor_retirement_during_original_preview_retires_unpublished_input() {
    interrupt_during_preview(false, Interruption::ActorRetirement, false, false).await;
}

#[tokio::test]
async fn fatal_original_preview_retires_input_and_staged_tool_installation() {
    interrupt_during_preview(false, Interruption::FatalPreview, true, false).await;
}

#[tokio::test]
async fn durable_cancellation_before_native_claim_preserves_public_owner_and_second_activation() {
    interrupt_during_preview(true, Interruption::Cancellation, false, true).await;
}

#[tokio::test]
async fn durable_fatal_preview_preserves_original_public_surface() {
    interrupt_during_preview(false, Interruption::FatalPreview, true, true).await;
}

#[derive(Clone, Copy)]
enum Interruption {
    Cancellation,
    TerminalRequest,
    ActorRetirement,
    FatalPreview,
}

async fn interrupt_during_preview(
    already_installed: bool,
    interruption: Interruption,
    genuine_preparation: bool,
    durable: bool,
) {
    let mut fixture = invocation_work::tests::Fixture::start().await;
    struct RunOwner {
        root: PathBuf,
        _lock: std::fs::File,
    }
    impl tidepool_runtime::session::RecoveryRunAuthority for RunOwner {
        fn owns_run(&self, root: &std::path::Path) -> std::io::Result<bool> {
            Ok(root.canonicalize()? == self.root)
        }
    }
    let durable_root = tempfile::tempdir().unwrap();
    let manifest = durable_root.path().join("declarations.json");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(durable_root.path().join("run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let authority = Arc::new(RunOwner {
        root: durable_root.path().canonicalize().unwrap(),
        _lock: lock,
    });
    let (mut resident, mut context, source, mut parked, _root) =
        crate::resident_workbench::request_tests::activation_session_fixture(|lib| {
            if durable {
                lib.attach_owned_recovery_graph_v3(&manifest, authority)
                    .unwrap();
            }
        });
    let (hole, input) = parked.remove(0);
    let second =
        (durable && matches!(interruption, Interruption::Cancellation)).then(|| parked.remove(0));
    for (unused_hole, unused_input) in parked {
        let _ = resident.abort(unused_hole.cont_id(), "unused activation".into());
        drop(unused_input);
    }
    let handles = resident.value_handle_count();
    context.actor = fixture.actor.identity();
    if durable {
        context.placement.lexical_scope = resident.mint_scope(ScopeId::ROOT).unwrap();
    }
    let machines = Arc::new(ActorMachineRegistry::new());
    fixture.environment.runner = ResidentActorRunner::new(Arc::clone(&machines), source);
    let owner_actor = ActorRef::first(crate::ActorId(context.actor.id.0 + 100));
    let request_id = fixture
        .environment
        .requests
        .reserve(owner_actor, context.actor);
    fixture
        .environment
        .requests
        .mark_queued(owner_actor, context.actor, request_id)
        .unwrap();
    let request = crate::interactive_session::InteractiveSessionRequest {
        type_evidence: input.type_evidence().clone(),
        request: request_id,
        initial_user_message: None,
        input_type: "Int -> Int".into(),
        response: crate::ResponseExpectation::new("()"),
        siblings: Vec::new(),
    };
    let mut descriptor = ActorDescriptor::new("activation cancellation", context.placement);
    let durable_owner = durable.then(|| {
        let path = tidepool_repr::ActorPath::parse("root/activation-cancellation").unwrap();
        descriptor = descriptor
            .clone()
            .with_actor_path(path.clone())
            .with_persistence_policy(crate::ActorPersistencePolicy::Durable);
        tidepool_runtime::session::RecoveryPublicOwner::new(&path, context.actor.incarnation.0)
            .unwrap()
    });
    if let Some(owner) = &durable_owner {
        resident
            .initialize_durable_public_scope(owner.clone(), context.placement.lexical_scope)
            .unwrap();
    }
    let original_public = resident
        .public_visibility_snapshot_in(context.placement.lexical_scope)
        .unwrap();
    let original_manifest = durable.then(|| std::fs::read(&manifest).unwrap());
    let readiness = durable_owner.as_ref().map(|owner| {
        resident
            .durable_public_readiness(owner, context.placement.lexical_scope)
            .unwrap()
    });
    machines.insert_idle(context.placement.session, Box::new(resident));
    let owner = WorkbenchPublicOwner::issue(&context, &descriptor, readiness).unwrap();
    let mut behavior = ResidentKernelBehavior::with_boot(
        descriptor.clone(),
        fixture.environment.clone(),
        ResidentBoot::Workbench,
        Vec::new(),
    );
    behavior.policy_installed = already_installed;
    fixture.environment.actors.lock().insert(
        context.actor,
        ResidentActorRecord {
            root_startup: None,
            public_owner: if durable {
                ActorPublicOwnerPlane::DurableReady(owner.clone())
            } else {
                ActorPublicOwnerPlane::Ephemeral(owner.clone())
            },
            recovery_claimed: false,
            workbench_executions: behavior.workbench_executions.clone(),
            forest_control: false,
            interactive_policy_installed: already_installed,
            observation_roots: Default::default(),
            descriptor: descriptor.clone(),
            bound_worktree: None,
            terminal: None,
            runtime_observation: behavior.runtime_observation.clone(),
            scheduler_root: true,
            displays: Default::default(),
        },
    );
    let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let staged_tools = Arc::new(Mutex::new(None));
    let observed_tools = Arc::clone(&staged_tools);
    behavior.activation_preview_observer = Some(Arc::new(move |observation| {
        use crate::resident_workbench::ActivationPublicationObservation;
        let mounted = match observation {
            ActivationPublicationObservation::InputMounted(mounted) => mounted,
            ActivationPublicationObservation::NativeClaimed
            | ActivationPublicationObservation::BeforeConfirmation
            | ActivationPublicationObservation::NativeSettled(_) => return Ok(()),
            ActivationPublicationObservation::ToolsPrepared {
                tools,
                dispatch,
                lexical,
                scope,
            } => {
                assert!(observed_tools
                    .lock()
                    .replace((tools, dispatch, lexical, scope))
                    .is_none());
                return Ok(());
            }
        };
        let certificate = mounted.interface().value_interface_certificate();
        entered
            .send((
                mounted.binding(),
                certificate.owner(),
                Arc::downgrade(&certificate),
                Arc::downgrade(mounted.interface()),
            ))
            .unwrap();
        release_rx
            .lock()
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("test must release original preview");
        if matches!(interruption, Interruption::FatalPreview) {
            return Err(ResidentActorWorkbenchError::InputCompilation {
                stage: crate::resident_workbench::ActivationCompileStage::Preview,
                error: tidepool_runtime::CompileError::ExtractFailed(
                    "injected fatal preview boundary".into(),
                ),
            });
        }
        Ok(())
    }));
    let genuine_installation = if matches!(interruption, Interruption::FatalPreview) {
        let installation = behavior
            .prepare_interactive_policy(&fixture.kernel, &context, None)
            .await
            .unwrap();
        let issued = installation
            .prepared_tools
            .as_ref()
            .expect("genuine installer issued its handler lease");
        let acquisition = issued
            .toolset_acquisition()
            .expect("genuine installer retains its source acquisition")
            .clone();
        assert_eq!(installation.toolset_acquisition(), Some(&acquisition));
        // The observer envelope keeps the exact origin after handler custody
        // leaves the envelope, without retaining the transferred lease.
        let mut observation = installation.clone();
        let transferred = observation.prepared_tools.take().unwrap();
        assert_eq!(
            observation.toolset_acquisition(),
            transferred.toolset_acquisition()
        );
        drop(transferred);
        assert_eq!(observation.toolset_acquisition(), Some(&acquisition));
        drop(observation);
        Some(installation)
    } else {
        None
    };
    let mut bootstrap = fixture
        .environment
        .runner
        .begin_public_bootstrap(context.clone(), owner.clone())
        .await
        .unwrap();
    let prepared = genuine_installation
        .map(|installation| PreparedInteractivePublication {
            owner: owner.clone(),
            bootstrap: bootstrap.take(),
            installation,
        })
        .or_else(|| {
            (!already_installed && !genuine_preparation).then(|| PreparedInteractivePublication {
                owner,
                bootstrap: bootstrap.take(),
                installation: LocalResidentInstallation {
                    prepared_tools: None,
                    toolset_acquisition: None,
                    actor: fixture.actor.clone(),
                    label: descriptor.label().into(),
                    policy: Arc::new(crate::ResidentInteractivePolicy::local(
                        fixture.actor.clone(),
                    )),
                    initial_user_message: None,
                    launch_worktrees: Vec::new(),
                    worktree_custody: None,
                    capabilities: descriptor.capabilities().clone(),
                    fork_effort: None,
                    model: None,
                    instructions: None,
                    creator: None,
                    fork_boundary: None,
                    checkpoint: None,
                    checkpoint_attachment: None,
                    supervisor_parent: None,
                    context_parent: None,
                    runtime_observation: behavior.runtime_observation.clone(),
                },
            })
        });
    let cancellation = async {
        let mounted = entered_rx.recv().await.expect("original input mounted");
        match interruption {
            Interruption::Cancellation => {
                let (outcome, notification) = fixture
                    .environment
                    .requests
                    .cancel_request(
                        owner_actor,
                        request_id,
                        crate::CancellationReason::RequesterCancelled,
                    )
                    .unwrap();
                assert_eq!(outcome, crate::CancelRequestOutcome::Requested);
                assert_eq!(notification.unwrap().request, request_id);
            }
            Interruption::TerminalRequest => {
                fixture.environment.requests.actor_stopped(
                    context.actor,
                    &ActorTerminal {
                        kind: crate::ActorExitKind::Cancelled,
                        summary: "terminal during original preview".into(),
                        diagnostic: None,
                    },
                );
            }
            Interruption::FatalPreview => {}
            Interruption::ActorRetirement => {
                fixture
                    .kernel
                    .retained_exit()
                    .request_shutdown(ActorTerminal {
                        kind: crate::ActorExitKind::Cancelled,
                        summary: "retirement during original preview".into(),
                        diagnostic: None,
                    });
            }
        }
        assert!(fixture.deployments.try_recv().is_err());
        release.send(()).unwrap();
        mounted
    };
    let (parked, (binding, module, certificate, interface)) = tokio::join!(
        behavior.park_interactive(
            &fixture.kernel,
            &context,
            crate::ResidentInteractiveSession {
                request,
                hole,
                input
            },
            prepared,
        ),
        cancellation,
    );
    match interruption {
        Interruption::Cancellation => assert!(
            matches!(parked.unwrap(), InteractivePark::Cancelled(actual) if actual == request_id)
        ),
        Interruption::TerminalRequest => assert!(matches!(
            parked,
            Err(ResidentActorWorkbenchError::ActorProtocol(_))
        )),
        Interruption::ActorRetirement => assert!(matches!(
            parked,
            Err(ResidentActorWorkbenchError::RetiredBeforeAdmission(_))
        )),
        Interruption::FatalPreview => {
            let Err(ResidentActorWorkbenchError::ActivationBindingCommitted {
                binding: actual,
                source,
            }) = parked
            else {
                panic!("fatal preview must preserve its committed binding and original error")
            };
            assert_eq!(actual, binding);
            assert!(matches!(
                *source,
                ResidentActorWorkbenchError::InputCompilation {
                    stage: crate::resident_workbench::ActivationCompileStage::Preview,
                    error: tidepool_runtime::CompileError::ExtractFailed(_),
                }
            ));
        }
    }
    assert!(fixture.deployments.try_recv().is_err());
    if genuine_preparation {
        assert_eq!(
            behavior.spec_installs, 1,
            "real installer completed before publication refusal"
        );
        assert!(behavior.installed_tools.current().is_none());
    }
    assert!(behavior.outstanding_interactive.is_none());
    assert!(matches!(behavior.standing, ResidentStanding::Boot));
    assert!(behavior.assignment_base.is_none());
    assert_eq!(behavior.policy_installed, already_installed);
    assert!(behavior
        .runtime_observation
        .snapshot()
        .request_activation
        .is_none());
    assert_eq!(
        fixture.environment.actors.lock()[&context.actor].interactive_policy_installed,
        already_installed,
    );
    let (mut resident, receipt) = machines
        .checkout_run(context.placement.session)
        .unwrap()
        .into_parts();
    resident.value_handle_count();
    assert_eq!(
        resident
            .public_visibility_snapshot_in(context.placement.lexical_scope)
            .unwrap(),
        original_public
    );
    if let Some(bytes) = original_manifest {
        assert_eq!(std::fs::read(&manifest).unwrap(), bytes);
    }
    if let Some(owner) = &durable_owner {
        resident
            .begin_durable_private_execution(owner, context.placement.lexical_scope)
            .expect("refused activation keeps the exact durable owner admissible");
    }
    assert!(resident
        .current_binding_in(context.placement.lexical_scope, "sessionInput")
        .is_none());
    assert!(resident
        .retain_binding_custody_in(context.placement.lexical_scope, "sessionInput", binding)
        .unwrap()
        .is_none());
    let view = resident
        .compile_view_in(context.placement.lexical_scope)
        .unwrap();
    assert!(!view.reachable_values().contains(&module));
    assert!(
        certificate.upgrade().is_none(),
        "cancelled input releases its checked interface"
    );
    assert!(interface.upgrade().is_none());
    // Even without an installer, the real preview installs a program whose
    // code roots persist until collection. Affine request custody must expire
    // at refusal independently of that compilation residency.
    if genuine_preparation {
        let (tools, dispatch, lexical, scope) = staged_tools
            .lock()
            .take()
            .expect("genuine preparation exposes its exact installation custody");
        assert!(tools.upgrade().is_none(), "refusal drops the staged tools");
        assert!(
            dispatch.upgrade().is_none(),
            "refusal drops the rooted dispatcher"
        );
        assert!(
            lexical.upgrade().is_none(),
            "refusal drops the exact lexical lease"
        );
        assert!(
            resident.compile_view_in(scope).is_none(),
            "dropped installer scope is retired"
        );
    } else {
        assert!(staged_tools.lock().is_none());
    }
    assert_eq!(
        resident.value_handle_count(),
        handles - 1,
        "refusal releases the adopted input handle and every temporary installer handle"
    );
    let cleanup_roots = resident.persistent_roots_count();
    resident.retire_binding_owner(binding);
    assert_eq!(
        resident.persistent_roots_count(),
        cleanup_roots,
        "exact cleanup is idempotent"
    );
    assert_eq!(resident.value_handle_count(), handles - 1);
    if second.is_none() {
        resident.close_realm(context.placement.resource_scope);
        assert_eq!(resident.parked_count(), 0);
        assert_eq!(resident.outstanding_custody(), 0);
    }
    let holes = resident
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    machines.settle_suspended(receipt, resident, holes);
    if matches!(interruption, Interruption::Cancellation) {
        fixture
            .environment
            .requests
            .begin_cancellation_acknowledgement(context.actor, request_id)
            .unwrap();
        fixture
            .environment
            .requests
            .finish_cancellation_acknowledgement(request_id);
        assert_eq!(
            fixture
                .environment
                .requests
                .observe_reply(context.actor, request_id),
            Ok(crate::ReplyObservation::Closed)
        );
    }
    if let Some((hole, input)) = second {
        behavior.activation_preview_observer = None;
        let second_request = fixture
            .environment
            .requests
            .reserve(owner_actor, context.actor);
        fixture
            .environment
            .requests
            .mark_queued(owner_actor, context.actor, second_request)
            .unwrap();
        let request = crate::interactive_session::InteractiveSessionRequest {
            type_evidence: input.type_evidence().clone(),
            request: second_request,
            initial_user_message: None,
            input_type: "Int -> Int".into(),
            response: crate::ResponseExpectation::new("()"),
            siblings: Vec::new(),
        };
        assert!(matches!(
            behavior
                .park_interactive(
                    &fixture.kernel,
                    &context,
                    crate::ResidentInteractiveSession {
                        request,
                        hole,
                        input
                    },
                    None
                )
                .await
                .unwrap(),
            InteractivePark::Parked
        ));
        let (mut resident, receipt) = machines
            .checkout_run(context.placement.session)
            .unwrap()
            .into_parts();
        resident
            .begin_durable_private_execution(
                durable_owner.as_ref().unwrap(),
                context.placement.lexical_scope,
            )
            .expect("second actual activation publishes a matching durable/local public surface");
        assert!(resident
            .current_binding_in(context.placement.lexical_scope, "sessionInput")
            .is_some());
        resident.close_realm(context.placement.resource_scope);
        let holes = resident
            .parked_holes()
            .into_iter()
            .map(str::to_owned)
            .collect();
        machines.settle_suspended(receipt, resident, holes);
    }
    fixture.finish().await;
}

#[tokio::test]
async fn durable_late_cancellation_retains_committed_input_and_suppresses_provider() {
    native_operation_sequence(true, NativeFailure::None, false).await;
}

#[tokio::test]
async fn ephemeral_late_cancellation_retains_committed_input_and_suppresses_provider() {
    native_operation_sequence(false, NativeFailure::None, false).await;
}

#[tokio::test]
async fn durable_waiter_drop_settles_native_request_before_join_delivery() {
    native_operation_sequence(true, NativeFailure::None, true).await;
}

#[tokio::test]
async fn ephemeral_waiter_drop_settles_native_request_before_join_delivery() {
    native_operation_sequence(false, NativeFailure::None, true).await;
}

#[tokio::test]
async fn durable_before_rename_failure_releases_request_fence_after_waiter_drop() {
    native_operation_sequence(true, NativeFailure::BeforeRename, true).await;
}

#[tokio::test]
async fn durable_unconfirmed_waiter_drop_fences_provider_and_request_until_exact_confirmation() {
    native_operation_sequence(true, NativeFailure::Confirmation, true).await;
}

#[derive(Clone, Copy)]
enum NativeFailure {
    None,
    BeforeRename,
    Confirmation,
}

async fn native_operation_sequence(durable: bool, failure: NativeFailure, drop_waiter: bool) {
    use crate::resident_workbench::ActivationPublicationObservation;
    use std::os::unix::fs::PermissionsExt;
    use tidepool_runtime::session::PublicManifestCommit;

    let mut fixture = invocation_work::tests::Fixture::start().await;
    struct RunOwner {
        root: PathBuf,
        _lock: std::fs::File,
    }
    impl tidepool_runtime::session::RecoveryRunAuthority for RunOwner {
        fn owns_run(&self, root: &std::path::Path) -> std::io::Result<bool> {
            Ok(root.canonicalize()? == self.root)
        }
    }
    let durable_root = tempfile::tempdir().unwrap();
    let manifest = durable_root.path().join("declarations.json");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(durable_root.path().join("run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let authority = Arc::new(RunOwner {
        root: durable_root.path().canonicalize().unwrap(),
        _lock: lock,
    });
    let (mut resident, mut context, source, mut parked, _source_root) =
        crate::resident_workbench::request_tests::activation_session_fixture(|lib| {
            if durable {
                lib.attach_owned_recovery_graph_v3(&manifest, authority)
                    .unwrap();
            }
        });
    let (hole, input) = parked.remove(0);
    for (hole, input) in parked {
        let _ = resident.abort(hole.cont_id(), "unused native fixture input".into());
        drop(input);
    }
    context.actor = fixture.actor.identity();
    if durable {
        context.placement.lexical_scope = resident.mint_scope(ScopeId::ROOT).unwrap();
    }
    let mut descriptor = ActorDescriptor::new("native activation", context.placement);
    let durable_owner = durable.then(|| {
        let path = tidepool_repr::ActorPath::parse("root/native-activation").unwrap();
        descriptor = descriptor
            .clone()
            .with_actor_path(path.clone())
            .with_persistence_policy(crate::ActorPersistencePolicy::Durable);
        tidepool_runtime::session::RecoveryPublicOwner::new(&path, context.actor.incarnation.0)
            .unwrap()
    });
    if let Some(owner) = &durable_owner {
        resident
            .initialize_durable_public_scope(owner.clone(), context.placement.lexical_scope)
            .unwrap();
    }
    let original_public = resident
        .public_visibility_snapshot_in(context.placement.lexical_scope)
        .unwrap();
    let original_manifest = durable.then(|| std::fs::read(&manifest).unwrap());
    let readiness = durable_owner.as_ref().map(|owner| {
        resident
            .durable_public_readiness(owner, context.placement.lexical_scope)
            .unwrap()
    });
    let owner = WorkbenchPublicOwner::issue(&context, &descriptor, readiness).unwrap();
    let machines = Arc::new(ActorMachineRegistry::new());
    machines.insert_idle(context.placement.session, Box::new(resident));
    fixture.environment.runner = ResidentActorRunner::new(machines.clone(), source);
    let mut behavior = ResidentKernelBehavior::with_boot(
        descriptor.clone(),
        fixture.environment.clone(),
        ResidentBoot::Workbench,
        Vec::new(),
    );
    behavior.policy_installed = true;
    fixture.environment.actors.lock().insert(
        context.actor,
        ResidentActorRecord {
            root_startup: None,
            public_owner: if durable {
                ActorPublicOwnerPlane::DurableReady(owner.clone())
            } else {
                ActorPublicOwnerPlane::Ephemeral(owner.clone())
            },
            recovery_claimed: false,
            workbench_executions: behavior.workbench_executions.clone(),
            forest_control: false,
            interactive_policy_installed: true,
            observation_roots: Default::default(),
            descriptor,
            bound_worktree: None,
            terminal: None,
            runtime_observation: behavior.runtime_observation.clone(),
            scheduler_root: true,
            displays: Default::default(),
        },
    );
    let requester = ActorRef::first(crate::ActorId(context.actor.id.0 + 100));
    let request_id = fixture
        .environment
        .requests
        .reserve(requester, context.actor);
    fixture
        .environment
        .requests
        .mark_queued(requester, context.actor, request_id)
        .unwrap();
    let request = crate::interactive_session::InteractiveSessionRequest {
        type_evidence: input.type_evidence().clone(),
        request: request_id,
        initial_user_message: None,
        input_type: "Int -> Int".into(),
        response: crate::ResponseExpectation::new("()"),
        siblings: Vec::new(),
    };
    enum NativeEvent {
        Claimed,
        Settled(PublicManifestCommit),
    }
    let (native_tx, mut native_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let root = durable_root.path().to_path_buf();
    behavior.activation_preview_observer = Some(Arc::new(move |observation| {
        match observation {
            ActivationPublicationObservation::NativeClaimed => {
                native_tx.send(NativeEvent::Claimed).unwrap();
                release_rx
                    .lock()
                    .recv_timeout(std::time::Duration::from_secs(60))
                    .unwrap();
                let mode = match failure {
                    NativeFailure::None => None,
                    NativeFailure::BeforeRename => Some(0o500),
                    // Rename needs write/execute, while parent-directory sync
                    // needs a readable directory. Native confirmation fails too.
                    NativeFailure::Confirmation => Some(0o300),
                };
                if let Some(mode) = mode {
                    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(mode)).unwrap();
                }
            }
            ActivationPublicationObservation::NativeSettled(commit) => {
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
                native_tx
                    .send(NativeEvent::Settled(commit.clone()))
                    .unwrap();
                // Native disposition is already settled, and the JoinHandle
                // cannot deliver while this owned blocking operation waits.
                release_rx
                    .lock()
                    .recv_timeout(std::time::Duration::from_secs(60))
                    .unwrap();
            }
            _ => {}
        }
        Ok(())
    }));
    let mut park = Some(Box::pin(behavior.park_interactive(
        &fixture.kernel,
        &context,
        crate::ResidentInteractiveSession {
            request,
            hole,
            input,
        },
        None,
    )));
    tokio::select! {
        event = native_rx.recv() => assert!(matches!(event, Some(NativeEvent::Claimed))),
        _result = park.as_mut().unwrap().as_mut() => panic!("activation ended before native claim"),
    }
    assert_eq!(
        fixture
            .environment
            .requests
            .begin_reply(context.actor, request_id),
        Err(crate::ReplyError::UpdatePending)
    );
    fixture
        .environment
        .requests
        .cancel_request(
            requester,
            request_id,
            crate::CancellationReason::RequesterCancelled,
        )
        .unwrap();
    assert_eq!(
        fixture
            .environment
            .requests
            .begin_cancellation_acknowledgement(context.actor, request_id),
        Err(crate::ReplyError::UpdatePending)
    );
    if drop_waiter {
        drop(park.take());
    }
    release_tx.send(()).unwrap();
    let NativeEvent::Settled(commit) = native_rx.recv().await.unwrap() else {
        panic!("expected native settlement");
    };
    let visible = !matches!(failure, NativeFailure::BeforeRename);
    match failure {
        NativeFailure::None => assert_eq!(
            commit,
            if durable {
                PublicManifestCommit::Durable
            } else {
                PublicManifestCommit::Ephemeral
            }
        ),
        NativeFailure::BeforeRename => {
            assert!(matches!(commit, PublicManifestCommit::BeforeRename { .. }))
        }
        NativeFailure::Confirmation => {
            let PublicManifestCommit::PublishedDurabilityUnconfirmed { detail } = &commit else {
                panic!("expected visible uncertainty: {commit:?}");
            };
            assert!(detail.contains("confirmation failed:"));
            assert!(!owner.is_ready());
            assert!(fixture.environment.actors.lock()[&context.actor]
                .public_owner
                .ready()
                .is_none());
            assert_eq!(
                fixture
                    .environment
                    .requests
                    .begin_cancellation_acknowledgement(context.actor, request_id),
                Err(crate::ReplyError::UpdatePending)
            );
        }
    }
    if !matches!(failure, NativeFailure::Confirmation) {
        fixture
            .environment
            .requests
            .begin_cancellation_acknowledgement(context.actor, request_id)
            .unwrap();
    }
    release_tx.send(()).unwrap();
    if let Some(park) = park.take() {
        assert!(
            matches!(park.await.unwrap(), InteractivePark::Cancelled(actual) if actual == request_id)
        );
    }
    drop(park);
    if !matches!(failure, NativeFailure::Confirmation) {
        fixture
            .environment
            .requests
            .finish_cancellation_acknowledgement(request_id);
    }
    if matches!(failure, NativeFailure::Confirmation) {
        let error = fixture
            .environment
            .runner
            .begin_private_execution(
                context.clone(),
                owner.clone(),
                tidepool_runtime::session::PublicationDecision::new(),
            )
            .await
            .err()
            .expect("unconfirmed native graph refuses private execution");
        assert!(matches!(
            error,
            ResidentActorWorkbenchError::Resident(ResidentError::Session(
                tidepool_runtime::session::SessionError::InvalidDurablePublicAdmission {
                    reason: tidepool_runtime::session::DurablePublicAdmissionFailure::Unconfirmed,
                    ..
                }
            ))
        ));
        fixture
            .environment
            .runner
            .confirm_durable_public_owner(context.clone(), durable_owner.clone().unwrap())
            .await
            .unwrap();
        assert!(owner.is_ready());
        fixture
            .environment
            .requests
            .begin_cancellation_acknowledgement(context.actor, request_id)
            .unwrap();
        fixture
            .environment
            .requests
            .finish_cancellation_acknowledgement(request_id);
    }
    // Acquire the actual next native owner entry; waiter delivery was optional.
    let private = fixture
        .environment
        .runner
        .begin_private_execution(
            context.clone(),
            owner.clone(),
            tidepool_runtime::session::PublicationDecision::new(),
        )
        .await
        .unwrap();
    drop(private);
    let (mut resident, receipt) = machines
        .checkout_run(context.placement.session)
        .unwrap()
        .into_parts();
    resident.value_handle_count();
    let current = resident
        .public_visibility_snapshot_in(context.placement.lexical_scope)
        .unwrap();
    assert_eq!(
        resident
            .current_binding_in(context.placement.lexical_scope, "sessionInput")
            .is_some(),
        visible
    );
    if visible {
        assert_ne!(current, original_public);
    } else {
        assert_eq!(current, original_public);
    }
    if let Some(bytes) = original_manifest {
        if visible {
            assert_ne!(std::fs::read(&manifest).unwrap(), bytes);
        } else {
            assert_eq!(std::fs::read(&manifest).unwrap(), bytes);
        }
    }
    assert!(fixture.deployments.try_recv().is_err());
    assert!(behavior.outstanding_interactive.is_none());
    assert!(behavior.assignment_base.is_none());
    assert!(matches!(behavior.standing, ResidentStanding::Boot));
    resident.close_realm(context.placement.resource_scope);
    let holes = resident
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    machines.settle_suspended(receipt, resident, holes);
    fixture.finish().await;
}
