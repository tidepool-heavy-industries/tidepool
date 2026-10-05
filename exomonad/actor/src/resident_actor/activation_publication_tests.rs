use super::*;

#[tokio::test]
async fn cancellation_during_original_preview_refuses_initial_provider_installation() {
    cancel_during_preview(false).await;
}

#[tokio::test]
async fn cancellation_during_original_preview_refuses_later_request_activation() {
    cancel_during_preview(true).await;
}

async fn cancel_during_preview(already_installed: bool) {
    let mut fixture = invocation_work::tests::Fixture::start().await;
    let (mut resident, mut context, source, mut parked, _root) =
        crate::resident_workbench::request_tests::activation_session_fixture(|_| {});
    let (hole, input) = parked.remove(0);
    for (unused_hole, unused_input) in parked {
        let _ = resident.abort(unused_hole.cont_id(), "unused activation".into());
        drop(unused_input);
    }
    let roots = resident.persistent_roots_count();
    context.actor = fixture.actor.identity();
    let machines = Arc::new(ActorMachineRegistry::new());
    machines.insert_idle(context.placement.session, Box::new(resident));
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
    let descriptor = ActorDescriptor::new("activation cancellation", context.placement);
    let owner = WorkbenchPublicOwner::issue(&context, &descriptor, None).unwrap();
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
            public_owner: ActorPublicOwnerPlane::Ephemeral(owner.clone()),
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
    let prepared = (!already_installed).then(|| PreparedInteractivePublication {
        owner,
        bootstrap: None,
        installation: LocalResidentInstallation {
            actor: fixture.actor.clone(),
            label: descriptor.label().into(),
            policy: Arc::new(crate::ResidentInteractivePolicy::local(
                fixture.actor.clone(),
            )),
            initial_user_message: None,
            launch_worktrees: Vec::new(),
            worktree_custody: None,
            effective_role: descriptor.effective_role().clone(),
            fork_effort: None,
            model: None,
            instructions: None,
            creator: None,
            fork_boundary: None,
            checkpoint: None,
            checkpoint_attachment: None,
            supervisor_parent: None,
            context_parent: None,
            fork_group: None,
            fork_gate: None,
            runtime_observation: behavior.runtime_observation.clone(),
        },
    });
    let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    behavior.activation_preview_observer = Some(Arc::new(move |mounted| {
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
    }));
    let cancellation = async {
        let mounted = entered_rx.recv().await.expect("original input mounted");
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
    assert!(matches!(parked.unwrap(), InteractivePark::Cancelled(actual) if actual == request_id));
    assert!(fixture.deployments.try_recv().is_err());
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
    assert_eq!(resident.persistent_roots_count(), roots);
    resident.retire_binding_owner(binding);
    assert_eq!(
        resident.persistent_roots_count(),
        roots,
        "exact cleanup is idempotent"
    );
    resident.close_realm(context.placement.resource_scope);
    assert_eq!(resident.parked_count(), 0);
    assert_eq!(resident.outstanding_custody(), 0);
    let holes = resident
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    machines.settle_suspended(receipt, resident, holes);
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
    fixture.finish().await;
}
