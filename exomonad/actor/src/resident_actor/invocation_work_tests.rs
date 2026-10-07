use super::*;
use crate::command_jobs::{CommandBackend, CommandControl};
use crate::request::{ResponseObservation, WatchRequirement, WorkbenchReservationAttempt};
use futures_util::future::BoxFuture;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandInput, CommandOutcome, CommandOutput, CommandPage,
    CommandPosition, CommandResult, CommandSpec, CommandStatus, CommandStream,
};

fn reservation() -> RequestReservationOwner {
    RequestReservationOwner::Workbench {
        execution: WorkbenchExecutionId::from_digest([7; 16]),
        attempt: WorkbenchReservationAttempt::fresh(),
    }
}

#[test]
fn nested_scope_tokens_fence_exact_owner_and_closed_ancestry() {
    let owner = ActorRef::first(crate::ActorId(1));
    let root = InvocationWork::new(owner, reservation());
    let sibling_root = InvocationWork::new(owner, reservation());
    let child = root.new_scope().unwrap();
    let nested = child.new_scope().unwrap();
    let sibling = root.new_scope().unwrap();
    let token = nested.scope_token().unwrap();
    assert_ne!(token, child.scope_token().unwrap());
    assert_ne!(token, sibling.scope_token().unwrap());
    assert!(Arc::ptr_eq(
        &root.find_scope(owner, token).unwrap(),
        &nested
    ));
    assert!(sibling_root.find_scope(owner, token).is_err());
    assert!(root
        .find_scope(ActorRef::first(crate::ActorId(2)), token)
        .is_err());
    assert!(root
        .find_scope(
            ActorRef {
                incarnation: crate::Incarnation(2),
                ..owner
            },
            token
        )
        .is_err());
    child.close();
    assert!(root.find_scope(owner, token).is_err());
    assert!(nested.with_admission(|| ()).is_err());
    assert!(nested.new_scope().is_err());
    assert!(sibling.with_admission(|| ()).is_ok());
    assert_eq!(root.scopes().len(), 2, "closed scope remains inspectable");
    root.close();
    assert!(sibling.with_admission(|| ()).is_err());
}

#[test]
fn scope_construction_provenance_is_independent_of_cleanup_transfer() {
    let owner = ActorRef::first(crate::ActorId(1));
    let root = InvocationWork::new(owner, reservation());
    let scope = root.new_scope().unwrap();
    let requests = RequestRegistry::default();
    let reserve = |work: &InvocationWork| {
        work.with_admission(|| {
            requests.reserve_for_cleanup_owner(
                owner,
                ActorRef::first(crate::ActorId(2)),
                "request".into(),
                true,
                Some(work.reservation_owner()),
                work.resource_cleanup_owner(),
            )
        })
        .unwrap()
    };
    let parent_request = reserve(&root);
    let child_request = reserve(&scope);
    scope
        .with_admission(|| {
            requests.transfer_request_cleanup_owner(
                owner,
                child_request,
                &scope.resource_cleanup_owner(),
                ResourceCleanupOwner::Actor,
            )
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        requests
            .abort_unsubmitted(owner, &scope.reservation_owner())
            .0,
        vec![child_request]
    );
    assert_eq!(
        requests
            .abort_unsubmitted(owner, &root.reservation_owner())
            .0,
        vec![parent_request]
    );
}

#[test]
fn escaped_scope_cannot_admit_after_parent_is_dropped_or_publishing() {
    let owner = ActorRef::first(crate::ActorId(1));
    let root = InvocationWork::new(owner, reservation());
    let scope = root.new_scope().unwrap();
    assert!(root.begin_publication());
    assert!(scope.with_admission(|| ()).is_err());
    drop(root);
    assert!(scope.with_admission(|| ()).is_err());
    assert!(scope.new_scope().is_err());
}

proptest::proptest! {
    #[test]
    fn scope_admission_matches_independent_ancestor_closure(
        parents in proptest::collection::vec(0usize..32, 1..24),
        close in 0usize..32,
    ) {
        let owner = ActorRef::first(crate::ActorId(1));
        let root = InvocationWork::new(owner, reservation());
        let mut nodes = vec![root.clone()];
        let mut parent_indices = vec![None];
        for selected in parents {
            let parent = selected % nodes.len();
            nodes.push(nodes[parent].new_scope().unwrap());
            parent_indices.push(Some(parent));
        }
        let closed = close % nodes.len();
        nodes[closed].close();
        for (index, node) in nodes.iter().enumerate() {
            let mut ancestor = Some(index);
            let mut expected_open = true;
            while let Some(current) = ancestor {
                if current == closed { expected_open = false; }
                ancestor = parent_indices[current];
            }
            proptest::prop_assert_eq!(node.with_admission(|| ()).is_ok(), expected_open);
            if let Some(token) = node.scope_token() {
                proptest::prop_assert_eq!(root.find_scope(owner, token).is_ok(), expected_open);
            }
        }
    }
}

#[test]
fn scope_transfer_gate_locks_shared_ancestry_once_and_refuses_closed_destination() {
    let owner = ActorRef::first(crate::ActorId(1));
    let root = InvocationWork::new(owner, reservation());
    let source = root.new_scope().unwrap();
    let target = root.new_scope().unwrap();
    let calls = AtomicUsize::new(0);
    source
        .with_transfer_admission(&target, || calls.fetch_add(1, Ordering::Relaxed))
        .unwrap();
    source
        .with_transfer_admission(&root, || calls.fetch_add(1, Ordering::Relaxed))
        .unwrap();
    source
        .with_transfer_admission(&source, || calls.fetch_add(1, Ordering::Relaxed))
        .unwrap();
    target.close();
    assert!(source
        .with_transfer_admission(&target, || calls.fetch_add(1, Ordering::Relaxed))
        .is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn nested_scope_cleanup_is_local_and_parent_cleanup_retains_confirmed_children() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let root = InvocationWork::new(owner, reservation());
    let scope = root.new_scope().unwrap();
    let nested = scope.new_scope().unwrap();
    let (parent_job, parent_backend) = fixture.pending_command().await;
    let (scope_job, scope_backend) = fixture.pending_command().await;
    let (nested_job, nested_backend) = fixture.pending_command().await;
    root.register_command_with_jobs(&fixture.environment.commands, &parent_job)
        .unwrap();
    root.register_command_with_jobs(&fixture.environment.commands, &scope_job)
        .unwrap();
    root.transfer_command_to_owner(&scope, &fixture.environment.commands, owner, &scope_job)
        .unwrap();
    nested
        .register_command_with_jobs(&fixture.environment.commands, &nested_job)
        .unwrap();
    fixture.cleanup(&scope).await;
    assert_eq!(scope_backend.cancellations.load(Ordering::Relaxed), 1);
    assert_eq!(nested_backend.cancellations.load(Ordering::Relaxed), 1);
    assert_eq!(parent_backend.cancellations.load(Ordering::Relaxed), 0);
    assert!(root.with_admission(|| ()).is_ok());
    assert!(scope.with_admission(|| ()).is_err());
    assert!(nested.with_admission(|| ()).is_err());
    fixture.cleanup(&root).await;
    assert_eq!(parent_backend.cancellations.load(Ordering::Relaxed), 1);
    assert_eq!(scope_backend.cancellations.load(Ordering::Relaxed), 1);
    assert_eq!(nested_backend.cancellations.load(Ordering::Relaxed), 1);
    let receipt = root.cleanup_observation().unwrap();
    assert_eq!(receipt.scopes.len(), 1);
    assert_eq!(receipt.scopes[0].1.scopes.len(), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn command_retention_moves_probe_cleanup_without_changing_actor_authority() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let root = InvocationWork::new(owner, reservation());
    let scope = root.new_scope().unwrap();
    let target = InvocationWork::new(owner, reservation());
    let (job, backend) = fixture.pending_command().await;
    let (probe, probe_backend) = fixture.pending_command().await;
    jobs.set_source_probe(&job, probe.clone()).unwrap();
    root.adopt_actor_command(jobs, owner, &job).unwrap();
    root.transfer_command_to_owner(&scope, jobs, owner, &job)
        .unwrap();
    assert_eq!(
        jobs.cleanup_owner(owner, &job).unwrap(),
        scope.resource_cleanup_owner()
    );
    assert_eq!(
        jobs.cleanup_owner(owner, &probe).unwrap(),
        scope.resource_cleanup_owner()
    );
    scope
        .transfer_command_to_owner(&target, jobs, owner, &job)
        .unwrap();
    assert_eq!(jobs.owner(&job).unwrap(), owner);
    assert_eq!(jobs.owner(&probe).unwrap(), owner);
    assert_eq!(
        jobs.cleanup_owner(owner, &job).unwrap(),
        target.resource_cleanup_owner()
    );
    assert_eq!(
        jobs.cleanup_owner(owner, &probe).unwrap(),
        target.resource_cleanup_owner()
    );
    fixture.cleanup(&root).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    assert_eq!(probe_backend.cancellations.load(Ordering::Relaxed), 0);
    fixture.cleanup(&target).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    assert_eq!(probe_backend.cancellations.load(Ordering::Relaxed), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn command_retention_refusal_preserves_cleanup_and_actor_retain_is_reversible() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let source = InvocationWork::new(owner, reservation());
    let closed = source.new_scope().unwrap();
    let target = source.new_scope().unwrap();
    let foreign = InvocationWork::new(ActorRef::first(crate::ActorId(999)), reservation());
    let (job, backend) = fixture.pending_command().await;
    source.register_command_with_jobs(jobs, &job).unwrap();
    closed.close();
    assert!(source
        .transfer_command_to_owner(&closed, jobs, owner, &job)
        .is_err());
    assert_eq!(
        source.transfer_command_to_owner(&foreign, jobs, owner, &job),
        Err(CommandError::CommandUnauthorized)
    );
    assert_eq!(
        source.transfer_command_to_actor(jobs, foreign.owner, &job),
        Err(CommandError::CommandUnauthorized)
    );
    assert_eq!(
        jobs.cleanup_owner(owner, &job).unwrap(),
        source.resource_cleanup_owner()
    );
    source.transfer_command_to_actor(jobs, owner, &job).unwrap();
    source.transfer_command_to_actor(jobs, owner, &job).unwrap();
    target.adopt_actor_command(jobs, owner, &job).unwrap();
    assert_eq!(
        source.transfer_command_to_actor(jobs, owner, &job),
        Err(CommandError::CommandUnauthorized)
    );
    assert_eq!(
        jobs.cleanup_owner(owner, &job).unwrap(),
        target.resource_cleanup_owner()
    );
    target
        .transfer_command_to_owner(&source, jobs, owner, &job)
        .unwrap();
    assert_eq!(jobs.owner(&job).unwrap(), owner);
    fixture.cleanup(&source).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn competing_command_retention_has_one_cleanup_owner() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let source = InvocationWork::new(owner, reservation());
    let left = source.new_scope().unwrap();
    let right = source.new_scope().unwrap();
    let (job, backend) = fixture.pending_command().await;
    source.register_command_with_jobs(jobs, &job).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|threads| {
        let a = threads.spawn(|| {
            barrier.wait();
            source.transfer_command_to_owner(&left, jobs, owner, &job)
        });
        let b = threads.spawn(|| {
            barrier.wait();
            source.transfer_command_to_owner(&right, jobs, owner, &job)
        });
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(CommandError::CommandUnauthorized))
            .count(),
        1
    );
    let winner = if results[0].is_ok() { &left } else { &right };
    let loser = if results[0].is_ok() { &right } else { &left };
    assert_eq!(
        jobs.cleanup_owner(owner, &job).unwrap(),
        winner.resource_cleanup_owner()
    );
    fixture.cleanup(loser).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    fixture.cleanup(winner).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    fixture.cleanup(&source).await;
    fixture.finish().await;
}

#[tokio::test]
async fn run_owned_command_and_probe_survive_actor_retirement_until_run_cleanup() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let work = InvocationWork::new(owner, reservation());
    let scope = work.new_scope().unwrap();
    let (job, backend) = fixture.pending_command().await;
    let (probe, probe_backend) = fixture.pending_command().await;
    jobs.set_source_probe(&job, probe.clone()).unwrap();
    scope.register_command_with_jobs(jobs, &job).unwrap();
    scope
        .transfer_command_to_run(jobs, &fixture.kernel, owner, &job)
        .unwrap();
    fixture.cleanup(&work).await;
    let retired = fixture
        .actor
        .shutdown_with_cleanup(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "issuing actor retired".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    assert!(retired.cleanup.is_confirmed());
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    assert_eq!(probe_backend.cancellations.load(Ordering::Relaxed), 0);
    assert_eq!(jobs.owner(&job).unwrap(), owner);
    assert_eq!(jobs.owner(&probe).unwrap(), owner);
    assert_eq!(
        jobs.cleanup_owner(owner, &job).unwrap(),
        ResourceCleanupOwner::Run
    );
    assert_eq!(
        jobs.cleanup_owner(owner, &probe).unwrap(),
        ResourceCleanupOwner::Run
    );
    assert!(!matches!(
        jobs.status(owner, &job).await.unwrap(),
        CommandStatus::CommandFinished(_)
    ));
    assert_eq!(
        fixture.directory.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    );
    assert!(backend.cancellations.load(Ordering::Relaxed) > 0);
    assert!(probe_backend.cancellations.load(Ordering::Relaxed) > 0);
    let cancellations = backend.cancellations.load(Ordering::Relaxed);
    assert_eq!(
        fixture.directory.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    );
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), cancellations);
    fixture.finish().await;
}

#[tokio::test]
async fn run_command_can_return_to_scope_and_refused_transfer_keeps_run_attachment() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let work = InvocationWork::new(owner, reservation());
    let closed = work.new_scope().unwrap();
    let target = work.new_scope().unwrap();
    let (job, backend) = fixture.pending_command().await;
    let (probe, probe_backend) = fixture.pending_command().await;
    jobs.set_source_probe(&job, probe.clone()).unwrap();
    target.register_command_with_jobs(jobs, &job).unwrap();
    target
        .transfer_command_to_run(jobs, &fixture.kernel, owner, &job)
        .unwrap();
    closed.close();
    assert!(closed
        .adopt_run_command(jobs, &fixture.kernel, owner, &job)
        .is_err());
    assert_eq!(
        jobs.cleanup_owner(owner, &job).unwrap(),
        ResourceCleanupOwner::Run
    );
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    target
        .adopt_run_command(jobs, &fixture.kernel, owner, &job)
        .unwrap();
    assert_eq!(
        jobs.cleanup_owner(owner, &job).unwrap(),
        target.resource_cleanup_owner()
    );
    assert!(!target.state.lock().detached_commands.contains(&job));
    assert!(!target.state.lock().detached_commands.contains(&probe));
    fixture.cleanup(&work).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    assert_eq!(
        fixture.directory.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    );
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    assert_eq!(probe_backend.cancellations.load(Ordering::Relaxed), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn closed_run_refuses_command_transfer_without_detaching_actor_cleanup() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let work = InvocationWork::new(owner, reservation());
    let (job, backend) = fixture.pending_command().await;
    work.register_command_with_jobs(jobs, &job).unwrap();
    fixture.directory.close_run_admission().await;
    assert!(work
        .transfer_command_to_run(jobs, &fixture.kernel, owner, &job)
        .is_err());
    assert_eq!(
        jobs.cleanup_owner(owner, &job).unwrap(),
        work.resource_cleanup_owner()
    );
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    fixture.cleanup(&work).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    assert_eq!(
        fixture.directory.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    );
    fixture.finish().await;
}

#[tokio::test]
async fn scope_worker_transfer_preserves_exact_identity_without_scope_retirement() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let root = InvocationWork::new(owner, reservation());
    let scope = root.new_scope().unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = fixture
        .kernel
        .spawn_worker(None, Owner::new(send), crate::WorkerLifetime::ActorOwned)
        .await
        .unwrap();
    let _worker_context = receive.await.unwrap();
    let child = worker.identity();
    scope.register_worker(worker.clone()).unwrap();
    assert!(scope
        .transfer_worker_to_actor(ActorRef::first(crate::ActorId(999)), child)
        .is_err());
    assert!(scope
        .transfer_worker_to_actor(
            owner,
            ActorRef {
                incarnation: crate::Incarnation(2),
                ..child
            }
        )
        .is_err());
    scope.transfer_worker_to_actor(owner, child).unwrap();
    scope.transfer_worker_to_actor(owner, child).unwrap();
    fixture.cleanup(&root).await;
    assert!(worker.terminal().get().is_none());
    assert!(!scope.owns_worker(child));
    worker
        .retire_by(
            owner,
            ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "test resource completed".into(),
                diagnostic: None,
            },
        )
        .await
        .unwrap();
    fixture.finish().await;
}

#[test]
fn scope_request_transfer_refuses_closed_parent_and_preserves_child_rollback() {
    let owner = ActorRef::first(crate::ActorId(1));
    let root = InvocationWork::new(owner, reservation());
    let scope = root.new_scope().unwrap();
    let target = root.new_scope().unwrap();
    let requests = RequestRegistry::default();
    let request = requests.reserve_for_cleanup_owner(
        owner,
        ActorRef::first(crate::ActorId(2)),
        "scope request".into(),
        true,
        Some(scope.reservation_owner()),
        scope.resource_cleanup_owner(),
    );
    target.close();
    assert_eq!(
        scope.transfer_request_to_owner(&target, &requests, owner, request),
        Err(crate::ReplyError::CancellationRequested)
    );
    scope
        .transfer_request_to_owner(&root, &requests, owner, request)
        .unwrap();
    assert_eq!(
        requests.cleanup_owner_requests(owner, &root.resource_cleanup_owner()),
        vec![request]
    );
    assert_eq!(
        requests
            .abort_unsubmitted(owner, &scope.reservation_owner())
            .0,
        vec![request]
    );
}

#[tokio::test]
async fn scope_realm_cleanup_failure_is_retained_for_parent_retry() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let root = InvocationWork::new(owner, reservation());
    let scope = root.new_scope().unwrap();
    let realm = RealmId::fresh();
    let descriptor = ActorDescriptor::new(
        "scope realm",
        crate::ActorPlacement {
            session: tidepool_repr::SessionId(99),
            resource_scope: RealmId::ROOT,
            lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
        },
    );
    let context = descriptor.session_context(owner);
    assert!(root.register_scope_realm(context.clone(), realm).is_err());
    assert!(scope
        .register_scope_realm(
            descriptor.session_context(ActorRef::first(crate::ActorId(999))),
            realm
        )
        .is_err());
    scope.register_scope_realm(context.clone(), realm).unwrap();
    scope.register_scope_realm(context, realm).unwrap();
    let first = root.cleanup(&fixture.environment, &fixture.kernel).await;
    assert!(first.uncertainty().unwrap().contains("realm"));
    assert_eq!(scope.state.lock().realms.len(), 1);
    let retry = root.cleanup(&fixture.environment, &fixture.kernel).await;
    assert!(retry.uncertainty().unwrap().contains("realm"));
    assert_eq!(scope.state.lock().realms.len(), 1);
    fixture.finish().await;
}

#[test]
fn invocation_membership_fences_actor_incarnation_and_reservation_attempt() {
    let owner = ActorRef::first(crate::ActorId(1));
    let reservation = reservation();
    let work = InvocationWork::new(owner, reservation.clone());
    assert!(work.matches(owner, &reservation));
    assert!(!work.matches(ActorRef::first(crate::ActorId(2)), &reservation));
    assert!(!work.matches(
        ActorRef {
            incarnation: crate::Incarnation(owner.incarnation.0 + 1),
            ..owner
        },
        &reservation,
    ));
    assert!(!work.matches(owner, &self::reservation()));
    assert!(!work.matches(owner, &RequestReservationOwner::Route(crate::WatchId(7))));

    let route = RequestReservationOwner::Route(crate::WatchId(7));
    let route_work = InvocationWork::new(owner, route.clone());
    assert!(route_work.matches(owner, &route));
    assert!(!route_work.matches(owner, &RequestReservationOwner::Route(crate::WatchId(8))));
    assert!(!route_work.matches(owner, &reservation));
}

fn compiler_owner(work: &Arc<InvocationWork>) -> crate::resident_workbench::CompilerCloseOwner {
    crate::resident_workbench::CompilerCloseOwner::Invocation {
        work: work.clone(),
        control: Some(crate::WorkbenchExecutionControl::untracked()),
    }
}

#[test]
fn publication_phase_admits_two_stage_receipts_and_fences_user_work() {
    use crate::local_actor::WorkerStartupAdmission;
    let actor = ActorRef::first(crate::ActorId(1));
    let work = InvocationWork::new(actor, reservation());
    let owner = compiler_owner(&work);
    assert!(owner.register_publication_work().is_err());
    assert!(work.begin_publication());
    assert!(!work.is_closed(), "publication precedes cleanup");
    assert!(!work.begin_publication(), "phase cannot restart");

    for generation in 0..2 {
        assert!(owner.register_work().is_err());
        assert!(work.register_command("late-command".into()).is_err());
        assert!(work.register_transient_watch(crate::WatchId(7)).is_err());
        assert!(work.register_group(crate::ForkGroupId(7)).is_err());
        assert!(work.reserve(ActorRef::first(crate::ActorId(2))).is_err());
        // A restaged generation owns its own close receipt. These lifecycle
        // controls perform no compiler command, so actual close is NotStarted.
        let ticket = owner.register_publication_work().unwrap();
        assert_eq!(work.state.lock().compilers.len(), generation + 1);
        let action = ticket.run(
            tidepool_runtime::CompilerTransactionCancellation::new(),
            || generation,
        );
        assert_eq!(action, generation);
    }
    let state = work.state.lock();
    assert!(state.compilers.iter().all(|receipt| matches!(
        receipt.observation(),
        crate::termination::CompilerWorkClose::Settled(
            tidepool_runtime::CompilerTransactionClose::NotStarted
        )
    )));
    assert!(state.commands.is_empty());
    assert!(state.watches.is_empty());
    assert!(state.groups.is_empty());
    assert!(state.pending_workers.is_empty());
    drop(state);
    work.close();
    assert!(work.is_closed());
    assert!(owner.register_publication_work().is_err());
    assert!(!work.begin_publication(), "closing never reopens admission");
}

#[tokio::test]
async fn publication_cleanup_retains_admitted_ticket_until_late_close_after_cancellation() {
    let fixture = Fixture::start().await;
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    let control = crate::WorkbenchExecutionControl::untracked();
    let owner = crate::resident_workbench::CompilerCloseOwner::Invocation {
        work: work.clone(),
        control: Some(control.clone()),
    };
    assert!(work.begin_publication());
    let ticket = owner.register_publication_work().unwrap();
    control.request_cancellation();
    let cleanup = work.cleanup(&fixture.environment, &fixture.kernel).await;
    assert!(work.is_closed());
    assert!(cleanup.uncertainty().is_some());
    assert!(matches!(
        control.compiler_close_observations().as_slice(),
        [crate::termination::CompilerWorkClose::Pending]
    ));
    assert!(owner.register_publication_work().is_err());
    assert!(owner.register_work().is_err());
    assert!(!work.begin_publication());
    // Closing fences new work but never erases the already admitted ticket.
    let action = ticket.run(
        tidepool_runtime::CompilerTransactionCancellation::new(),
        || Err::<(), _>("original publication refusal"),
    );
    assert_eq!(action, Err("original publication refusal"));
    assert!(cleanup.uncertainty().is_none());
    fixture.cleanup(&work).await;
    fixture.finish().await;
}

#[tokio::test]
async fn publication_failure_before_first_stage_closes_without_pending_obligation() {
    let fixture = Fixture::start().await;
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    let owner = compiler_owner(&work);
    assert!(work.begin_publication());
    work.close();
    assert!(work.state.lock().compilers.is_empty());
    assert!(owner.register_publication_work().is_err());
    fixture.cleanup(&work).await;
    fixture.finish().await;
}

struct Owner {
    context: Option<tokio::sync::oneshot::Sender<KernelContext>>,
    startup_gate: Option<Arc<ShutdownGate>>,
    shutdown_gate: Option<Arc<ShutdownGate>>,
    requests: Option<Arc<crate::request::RequestRegistry>>,
}

impl Owner {
    fn new(context: tokio::sync::oneshot::Sender<KernelContext>) -> Self {
        Self {
            context: Some(context),
            startup_gate: None,
            shutdown_gate: None,
            requests: None,
        }
    }
}

struct ShutdownGate {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    calls: AtomicUsize,
}

impl crate::KernelBehavior for Owner {
    fn start<'a>(
        &'a mut self,
        context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<crate::KernelStep<()>, crate::KernelBehaviorError>> {
        assert!(self.context.take().unwrap().send(context.clone()).is_ok());
        let gate = self.startup_gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.calls.fetch_add(1, Ordering::Relaxed);
                gate.entered.add_permits(1);
                gate.release.acquire().await.unwrap().forget();
            }
            Ok(crate::KernelStep::Continue(()))
        })
    }

    fn cast<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: crate::MailboxValue,
    ) -> BoxFuture<'a, Result<crate::KernelStep<()>, crate::KernelBehaviorError>> {
        Box::pin(async { panic!("fixture has no mailbox casts") })
    }

    fn call<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: crate::CallAncestry,
        _: crate::MailboxValue,
    ) -> BoxFuture<'a, Result<crate::KernelStep<crate::MailboxValue>, crate::KernelBehaviorError>>
    {
        Box::pin(async { panic!("fixture has no mailbox calls") })
    }

    fn tool<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: exomonad_tool::ToolInvocation,
        _: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> BoxFuture<'a, Result<crate::KernelStep<serde_json::Value>, crate::KernelInvocationFailure>>
    {
        Box::pin(async { panic!("fixture has no tools") })
    }

    fn workbench<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: crate::ActorWorkbenchInvocation,
        _: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> BoxFuture<'a, Result<crate::KernelStep<WorkbenchResponse>, crate::KernelInvocationFailure>>
    {
        Box::pin(async { panic!("fixture has no workbench") })
    }

    fn external_application_failed<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: crate::ExternalApplicationFailure,
    ) -> BoxFuture<'a, crate::ExternalFailureDisposition> {
        Box::pin(async { panic!("fixture has no external application") })
    }

    fn shutdown<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> BoxFuture<'a, Result<(), crate::KernelBehaviorError>> {
        let gate = self.shutdown_gate.clone();
        let requests = self.requests.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.calls.fetch_add(1, Ordering::Relaxed);
                gate.entered.add_permits(1);
                gate.release.acquire().await.unwrap().forget();
            }
            if let Some(requests) = requests {
                requests.actor_stopped(context.identity(), terminal);
            }
            Ok(())
        })
    }

    fn shutdown_components<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
        _: tokio::time::Instant,
    ) -> BoxFuture<
        'a,
        (
            crate::CleanupComponentOutcome,
            crate::CleanupComponentOutcome,
        ),
    > {
        Box::pin(async move {
            let hook = match self.shutdown(context, terminal).await {
                Ok(()) => crate::CleanupComponentOutcome::Confirmed,
                Err(error) => crate::CleanupComponentOutcome::Unconfirmed(error.to_string()),
            };
            // This fixture owns no resident machine, resource scope, or host
            // resources. Its real shutdown gate still controls hook evidence.
            (hook, crate::CleanupComponentOutcome::Confirmed)
        })
    }

    fn stopped<'a>(&'a mut self, _: &'a KernelContext, _: &'a ActorTerminal) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn child_exited(&mut self, _: crate::ChildExitNotice) {}
}

pub(in crate::resident_actor) struct Fixture {
    pub(in crate::resident_actor) actor: LocalActorRef,
    task: ractor::concurrency::JoinHandle<()>,
    directory: crate::LocalActorDirectory,
    pub(in crate::resident_actor) kernel: KernelContext,
    pub(in crate::resident_actor) environment:
        ResidentEnvironment<frunk::HNil, tidepool_mcp::CapturedOutput>,
    pub(in crate::resident_actor) deployments: mpsc::Receiver<LocalResidentDeployment>,
}

impl Fixture {
    pub(in crate::resident_actor) async fn start() -> Self {
        let (send, receive) = tokio::sync::oneshot::channel();
        let directory = crate::LocalActorDirectory::default();
        let (actor, task) = crate::local_actor::spawn_local_actor_in_directory(
            None,
            Owner::new(send),
            crate::Incarnation(1),
            directory.clone(),
        )
        .await
        .unwrap();
        let kernel = receive.await.unwrap();
        let (deployments, receiver) = mpsc::channel(DEPLOYMENT_CHANNEL_CAPACITY);
        let environment = ResidentEnvironment {
            runner: ResidentActorRunner::new(
                Arc::new(ActorMachineRegistry::new()),
                ActorWorkbenchSource::new("", Vec::new()),
            ),
            deployments,
            retired: Default::default(),
            requests: Default::default(),
            commands: Default::default(),
            fork_groups: crate::ForkGroupRegistry::new(crate::ActorLineageRegistry::default()),
            actors: Default::default(),
            fork_workspaces: None,
            root_admission_closed: Default::default(),
            launch_resolver: None,
            source_layers: None,
            jev: Arc::new(crate::jev::UnconfiguredJev),
            cell_model_factory: None,
            release_tracked: Default::default(),
            conversation_reader: None,
            usage_pointers: Default::default(),
            recovery: None,
        };
        Self {
            actor,
            task,
            directory,
            kernel,
            environment,
            deployments: receiver,
        }
    }

    async fn cleanup(&self, work: &InvocationWork) {
        let cleanup = tokio::time::timeout(
            Duration::from_secs(1),
            work.cleanup(&self.environment, &self.kernel),
        )
        .await
        .expect("responsive owners must finish invocation cleanup");
        assert_eq!(cleanup.uncertainty(), None);
        assert_eq!(work.cleanup_observation().unwrap().uncertainty(), None);
    }

    async fn pending_command(&self) -> (String, Arc<PendingBackend>) {
        let (job, request) = self
            .environment
            .commands
            .start(
                &self.kernel,
                CommandSpec {
                    argv: vec!["fixture-command".into()],
                    directory: None,
                    environment: Vec::new(),
                    memory: 64 * 1024 * 1024,
                    input: CommandInput::ClosedInput,
                    source_capture: tidepool_bridge_effects::CommandSourceCapture::NoCapture,
                },
                None,
            )
            .await
            .unwrap();
        let backend = Arc::new(PendingBackend::default());
        request.supply(Ok(backend.clone()));
        tokio::time::timeout(
            Duration::from_secs(1),
            self.environment.commands.supplied(&job),
        )
        .await
        .unwrap()
        .unwrap();
        (job, backend)
    }

    pub(in crate::resident_actor) async fn finish(self) {
        assert_eq!(
            self.directory.shutdown_run_resources().await,
            crate::CleanupComponentOutcome::Confirmed
        );
        self.actor
            .retire_by(
                self.actor.identity(),
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "invocation fixture complete".into(),
                    diagnostic: None,
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn closed_invocation_refuses_every_work_registration() {
    let fixture = Fixture::start().await;
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    work.close();
    assert!(matches!(
        work.register_command("late-command".into()),
        Err(CommandError::CommandUnavailable(_))
    ));
    assert_eq!(
        work.register_transient_watch(crate::WatchId(1)),
        Err(crate::ReplyError::CancellationRequested)
    );
    assert!(work.register_worker(fixture.actor.clone()).is_err());
    assert!(work.register_group(crate::ForkGroupId(1)).is_err());
    fixture.cleanup(&work).await;
    assert!(
        fixture.actor.terminal().get().is_none(),
        "refused worker registration must not retire it"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn invocation_cleanup_releases_transient_watch_without_cancelling_target() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let target = ActorRef::first(crate::ActorId(owner.id.0 + 100));
    let requests = &fixture.environment.requests;
    let request = requests.reserve(owner, target);
    requests.mark_queued(owner, target, request).unwrap();
    requests.present(target, request).unwrap();
    let watch = requests
        .register_transient_watch(
            owner,
            crate::request::test_readiness_groups(vec![vec![(
                request,
                WatchRequirement::Response {
                    allow_failure: false,
                },
            )]]),
        )
        .unwrap();
    let subscription = requests.subscribe_watch(owner, watch).unwrap();
    let work = InvocationWork::new(owner, reservation());
    work.register_transient_watch(watch).unwrap();

    fixture.cleanup(&work).await;

    assert!(!requests.retains_watch(owner, watch));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), subscription.wait())
            .await
            .unwrap(),
        Err(crate::ReplyError::Stale)
    );
    assert!(matches!(
        requests.observe_response(owner, request),
        Ok(ResponseObservation::Pending(_))
    ));
    requests
        .begin_reply(target, request)
        .expect("target remains able to reply");
    requests.finish_reply(request, None);
    assert_eq!(
        requests.observe_response(owner, request),
        Ok(ResponseObservation::Ready)
    );
    fixture.cleanup(&work).await;
    fixture.finish().await;
}

struct PendingBackend {
    cancellations: AtomicUsize,
    finish: tokio::sync::Semaphore,
    cancel_entered: tokio::sync::Semaphore,
    cancel_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}

impl Default for PendingBackend {
    fn default() -> Self {
        Self {
            cancellations: AtomicUsize::new(0),
            finish: tokio::sync::Semaphore::new(0),
            cancel_entered: tokio::sync::Semaphore::new(0),
            cancel_gate: Mutex::new(None),
        }
    }
}

impl CommandBackend for PendingBackend {
    fn execute<'a>(
        &'a self,
        _: &'a str,
        _: CommandSpec,
        _: tokio::sync::watch::Sender<CommandStatus>,
    ) -> BoxFuture<'a, CommandResult> {
        Box::pin(async move {
            self.finish.acquire().await.unwrap().forget();
            CommandResult {
                outcome: CommandOutcome::CommandExited(0),
                cleanup: CommandCleanup::CommandClean,
            }
        })
    }

    fn control<'a>(
        &'a self,
        _: &'a str,
        operation: CommandControl,
    ) -> BoxFuture<'a, Result<(), CommandError>> {
        Box::pin(async move {
            assert!(matches!(operation, CommandControl::Cancel));
            self.cancellations.fetch_add(1, Ordering::Relaxed);
            self.cancel_entered.add_permits(1);
            let gate = self.cancel_gate.lock().clone();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            self.finish.add_permits(1);
            Ok(())
        })
    }

    fn output<'a>(
        &'a self,
        _: &'a str,
        _: usize,
    ) -> BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async { panic!("fixture has no output") })
    }

    fn read<'a>(
        &'a self,
        _: &'a str,
        _: CommandStream,
        _: CommandPosition,
    ) -> BoxFuture<'a, Result<CommandPage, CommandError>> {
        Box::pin(async { panic!("fixture has no output pages") })
    }

    fn cleanup<'a>(&'a self, _: &'a str) -> BoxFuture<'a, CommandCleanup> {
        Box::pin(async { CommandCleanup::CommandClean })
    }
}

#[tokio::test]
async fn owned_command_detach_survives_cleanup_and_borrowed_detach_is_unauthorized() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let (job, backend) = fixture.pending_command().await;
    let work = InvocationWork::new(owner, reservation());
    work.register_command_with_jobs(&fixture.environment.commands, &job)
        .unwrap();
    let borrower = ActorRef::first(crate::ActorId(owner.id.0 + 100));
    assert_eq!(
        jobs.control(borrower, &job, CommandControl::Cancel).await,
        Err(CommandError::CommandUnauthorized)
    );
    assert_eq!(
        work.detach_command(jobs, borrower, &job),
        Err(CommandError::CommandUnauthorized)
    );
    assert_eq!(
        work.detach_command(
            jobs,
            ActorRef {
                incarnation: crate::Incarnation(owner.incarnation.0 + 1),
                ..owner
            },
            &job
        ),
        Err(CommandError::CommandUnauthorized)
    );
    let borrowed_work = InvocationWork::new(borrower, reservation());
    assert_eq!(
        borrowed_work.detach_command(jobs, borrower, &job),
        Err(CommandError::CommandUnauthorized)
    );
    work.detach_command(jobs, owner, &job).unwrap();

    fixture.cleanup(&work).await;

    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    assert_eq!(jobs.owner(&job), Ok(owner));
    assert!(!matches!(
        jobs.status(owner, &job).await.unwrap(),
        CommandStatus::CommandFinished(_)
    ));
    backend.finish.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), jobs.finished(owner, &job))
            .await
            .unwrap()
            .unwrap()
            .cleanup,
        CommandCleanup::CommandClean
    );
    fixture.finish().await;
}

#[tokio::test]
async fn foreign_actor_cannot_wait_for_command_cleanup() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let (job, backend) = fixture.pending_command().await;
    let foreign = ActorRef {
        incarnation: crate::Incarnation(owner.incarnation.0 + 1),
        ..owner
    };

    assert_eq!(
        jobs.finished(foreign, &job).await.unwrap_err(),
        CommandError::CommandUnauthorized
    );
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    assert!(!matches!(
        jobs.status(owner, &job).await.unwrap(),
        CommandStatus::CommandFinished(_)
    ));

    backend.finish.add_permits(1);
    assert_eq!(
        jobs.finished(owner, &job).await.unwrap().cleanup,
        CommandCleanup::CommandClean
    );
    fixture.finish().await;
}

#[tokio::test(start_paused = true)]
async fn blocked_settlement_notice_does_not_prevent_invocation_cancellation() {
    let mut fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let (job, backend) = fixture.pending_command().await;
    let work = InvocationWork::new(owner, reservation());
    work.register_command_with_jobs(&fixture.environment.commands, &job)
        .unwrap();

    let notice = fixture.environment.requests.reserve_command_settlement(
        owner,
        "retained-report".into(),
        true,
    );
    fixture
        .environment
        .requests
        .settle_command(notice, "completed".into(), None, None);
    assert!(fixture.environment.requests.has_settlement_notifications());

    for index in 0..DEPLOYMENT_CHANNEL_CAPACITY {
        fixture
            .environment
            .deployments
            .try_send(LocalResidentDeployment::Retired {
                actor: ActorRef::first(crate::ActorId(10_000 + index as u64)),
                terminal: ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "test channel filler".into(),
                    diagnostic: None,
                },
            })
            .unwrap();
    }

    let first = {
        let cleanup = work.cleanup(&fixture.environment, &fixture.kernel);
        tokio::pin!(cleanup);
        assert!(futures_util::poll!(cleanup.as_mut()).is_pending());
        let mut completed = None;
        for _ in 0..4 {
            tokio::time::advance(RELEASE_WAIT).await;
            if let std::task::Poll::Ready(result) = futures_util::poll!(cleanup.as_mut()) {
                completed = Some(result);
                break;
            }
            if backend.cancel_entered.available_permits() > 0 {
                break;
            }
        }
        backend
            .cancel_entered
            .try_acquire()
            .expect("a blocked notice flush must not delay command cancellation")
            .forget();
        assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
        assert!(fixture.environment.requests.has_settlement_notifications());

        if let Some(completed) = completed {
            completed
        } else {
            backend.finish.add_permits(1);
            tokio::time::advance(RELEASE_WAIT * 2).await;
            tokio::time::timeout(Duration::from_secs(1), &mut cleanup)
                .await
                .unwrap()
        }
    };
    assert!(first.settlement_notifications_pending);
    assert!(fixture.environment.requests.has_settlement_notifications());

    while fixture.deployments.try_recv().is_ok() {}
    let retried = work.cleanup(&fixture.environment, &fixture.kernel).await;
    assert!(!retried.settlement_notifications_pending);
    assert!(!fixture.environment.requests.has_settlement_notifications());
    fixture.finish().await;
}

#[tokio::test]
async fn sibling_invocation_cannot_detach_same_actor_command() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let (job, backend) = fixture.pending_command().await;
    let first = InvocationWork::new(owner, reservation());
    let sibling = InvocationWork::new(owner, reservation());
    sibling
        .register_command_with_jobs(&fixture.environment.commands, &job)
        .unwrap();

    assert_eq!(
        first.detach_command(jobs, owner, &job),
        Err(CommandError::CommandUnauthorized)
    );
    sibling.detach_command(jobs, owner, &job).unwrap();
    sibling
        .detach_command(jobs, owner, &job)
        .expect("own detach is idempotent");
    fixture.cleanup(&first).await;
    fixture.cleanup(&sibling).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    backend.finish.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), jobs.finished(owner, &job))
        .await
        .unwrap()
        .unwrap();
    fixture.finish().await;
}

#[tokio::test]
async fn concurrent_invocation_cleanup_cancels_owned_command_once() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let (job, backend) = fixture.pending_command().await;
    let work = InvocationWork::new(owner, reservation());
    work.register_command_with_jobs(&fixture.environment.commands, &job)
        .unwrap();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *backend.cancel_gate.lock() = Some(gate.clone());

    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(fixture.cleanup(&work), fixture.cleanup(&work), async {
            backend.cancel_entered.acquire().await.unwrap().forget();
            tokio::task::yield_now().await;
            gate.add_permits(1);
        },);
    })
    .await
    .expect("concurrent cleanup must share the completed observation");

    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    *backend.cancel_gate.lock() = None;
    fixture.finish().await;
}

#[tokio::test]
async fn invocation_cleanup_preserves_completed_worker_terminal() {
    let fixture = Fixture::start().await;
    let (send, receive) = tokio::sync::oneshot::channel();
    let (worker, task) = crate::spawn_local_actor(None, Owner::new(send))
        .await
        .unwrap();
    let _worker_context = receive.await.unwrap();
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    work.register_worker(worker.clone()).unwrap();
    let completed = ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "worker result retained".into(),
        diagnostic: None,
    };
    let shutdown = worker
        .shutdown_with_cleanup(completed.clone())
        .await
        .unwrap();
    assert_eq!(shutdown.terminal, completed);
    assert!(shutdown.cleanup.is_confirmed());
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();

    fixture.cleanup(&work).await;

    assert_eq!(worker.terminal().get(), Some(completed));
    assert!(worker.terminal().cleanup().unwrap().is_confirmed());
    fixture.finish().await;
}

#[test]
fn unconfirmed_invocation_cleanup_preserves_committed_reply_and_receipts() {
    let cleanup = InvocationCleanup {
        commands: vec![InvocationCommandCleanup {
            job: "retained-command".into(),
            result: Some(CommandResult {
                outcome: CommandOutcome::CommandExited(0),
                cleanup: CommandCleanup::CommandRetained,
            }),
            failure: None,
        }],
        ..Default::default()
    };
    let receipt = WorkbenchItemReceipt {
        index: 0,
        kind: None,
        span: None,
        source_items: Vec::new(),
        status: WorkbenchItemStatus::Committed,
        output: "committed reply".into(),
        value: None,
        diagnostics: Vec::new(),
        failure_layer: None,
        warnings: Vec::new(),
        installed_bindings: vec!["answer".into()],
        operations: Vec::new(),
        terminal_transfer: None,
    };
    let response = WorkbenchResponse {
        status: WorkbenchRunStatus::Committed,
        summary: Some("reply accepted".into()),
        items: vec![receipt.clone()],
        next_index: 1,
        total: 1,
        publication: None,
    };
    let terminal = ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "reply complete".into(),
        diagnostic: None,
    };
    for (expected_variant, step) in [
        (0, KernelStep::Continue(response.clone())),
        (1, KernelStep::ContinueLater(response.clone())),
        (
            2,
            KernelStep::Stop {
                output: response.clone(),
                terminal: terminal.clone(),
            },
        ),
    ] {
        let result = retain_invocation_cleanup_summary(Ok(step), cleanup.uncertainty());
        let Ok(step) = result else {
            panic!("committed reply remains successful")
        };
        let (variant, response) = match step {
            KernelStep::Continue(response) => (0, response),
            KernelStep::ContinueLater(response) => (1, response),
            KernelStep::Stop {
                output,
                terminal: actual_terminal,
            } => {
                assert_eq!(actual_terminal, terminal);
                (2, output)
            }
        };
        assert_eq!(variant, expected_variant);
        assert_eq!(response.status, WorkbenchRunStatus::Committed);
        assert_eq!(response.items, vec![receipt.clone()]);
        assert_eq!((response.next_index, response.total), (1, 1));
        let summary = response.summary.unwrap();
        assert!(summary.starts_with("reply accepted\n"));
        assert!(summary.contains("retained-command"));
    }
    assert!(
        cleanup.uncertainty().is_some(),
        "presenting uncertainty cannot settle the typed cleanup observation"
    );
    assert!(matches!(
        &cleanup.commands[0].result,
        Some(CommandResult {
            cleanup: CommandCleanup::CommandRetained,
            ..
        })
    ));
}

#[tokio::test]
async fn invocation_cleanup_cancels_owned_command_once_and_closes_detach() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let (job, backend) = fixture.pending_command().await;
    let work = InvocationWork::new(owner, reservation());
    work.register_command_with_jobs(&fixture.environment.commands, &job)
        .unwrap();
    work.register_command_with_jobs(&fixture.environment.commands, &job)
        .unwrap();
    work.close();
    assert!(matches!(
        work.detach_command(jobs, owner, &job),
        Err(CommandError::CommandUnavailable(_))
    ));

    fixture.cleanup(&work).await;

    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    assert!(matches!(
        jobs.status(owner, &job).await.unwrap(),
        CommandStatus::CommandFinished(CommandResult {
            cleanup: CommandCleanup::CommandClean,
            ..
        })
    ));
    fixture.cleanup(&work).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    fixture.finish().await;
}

fn group_with_child(fixture: &Fixture, child: ActorRef) -> crate::ForkGroupId {
    let owner = fixture.actor.identity();
    let groups = &fixture.environment.fork_groups;
    let (group, reservations) = groups
        .begin(
            owner,
            crate::ActorPath::parse("root/cleanup").unwrap(),
            vec![crate::ActorPathSegment::new("child").unwrap()],
            4,
        )
        .unwrap();
    groups
        .claim(group, owner, &reservations[0].allocated)
        .unwrap();
    groups.attach_child(group, owner, child).unwrap();
    group
}

#[tokio::test]
async fn interrupted_group_cleanup_retains_discovered_actor_owned_worker_for_retry() {
    let fixture = Fixture::start().await;
    let gate = Arc::new(ShutdownGate {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = fixture
        .kernel
        .spawn_worker(
            None,
            Owner {
                context: Some(send),
                startup_gate: None,
                shutdown_gate: Some(gate.clone()),
                requests: None,
            },
            crate::WorkerLifetime::ActorOwned,
        )
        .await
        .unwrap();
    let _worker_context = receive.await.unwrap();
    let child = worker.identity();
    let group = group_with_child(&fixture, child);
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    work.register_group(group).unwrap();
    assert!(
        !work.owns_worker(child),
        "group is the only invocation membership before abort"
    );

    let mut first_cleanup = Box::pin(work.cleanup(&fixture.environment, &fixture.kernel));
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = &mut first_cleanup => panic!("worker shutdown is still held"),
            permit = gate.entered.acquire() => permit.unwrap().forget(),
        }
    })
    .await
    .unwrap();
    assert!(work
        .state
        .lock()
        .workers
        .iter()
        .any(|worker| worker.identity() == child));
    assert!(work.cleanup_observation().is_none());
    drop(first_cleanup);
    assert!(matches!(
        fixture
            .environment
            .fork_groups
            .abort(group, fixture.actor.identity()),
        Err(crate::ForkGroupError::Unknown(_))
    ));
    gate.release.add_permits(1);

    fixture.cleanup(&work).await;

    let cleanup = work.cleanup_observation().unwrap();
    assert_eq!(cleanup.workers.len(), 1);
    assert_eq!(cleanup.workers[0].actor, child);
    assert_eq!(
        cleanup.workers[0].kernel,
        Ok(worker.terminal().cleanup().unwrap())
    );
    assert_eq!(
        worker.terminal().get().unwrap().kind,
        ActorExitKind::Cancelled
    );
    assert_eq!(gate.calls.load(Ordering::Relaxed), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn unresolved_aborted_group_worker_remains_uncertain_across_cleanup_retries() {
    let fixture = Fixture::start().await;
    let missing = ActorRef {
        id: crate::ActorId(fixture.actor.identity().id.0 + 1_000_000),
        incarnation: crate::Incarnation(2),
    };
    assert!(fixture.kernel.resolve(missing).is_none());
    let group = group_with_child(&fixture, missing);
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    work.register_group(group).unwrap();
    for _ in 0..2 {
        let cleanup = tokio::time::timeout(
            Duration::from_secs(1),
            work.cleanup(&fixture.environment, &fixture.kernel),
        )
        .await
        .unwrap();
        assert!(
            cleanup.uncertainty().is_some(),
            "missing worker cannot become confirmed by losing the group row"
        );
        assert!(work.state.lock().unresolved_workers.contains(&missing));
        assert!(work.cleanup_observation().unwrap().uncertainty().is_some());
    }
    fixture.finish().await;
}

#[tokio::test]
async fn invocation_request_cleanup_retains_target_acknowledgment_without_retiring_worker() {
    let mut fixture = Fixture::start().await;
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = fixture
        .kernel
        .spawn_child(None, Owner::new(send))
        .await
        .unwrap();
    let _worker_context = receive.await.unwrap();
    let owner = fixture.actor.identity();
    let target = worker.identity();
    let work = InvocationWork::new(owner, reservation());
    let requests = &fixture.environment.requests;
    let request = requests.reserve_for_operation(
        owner,
        target,
        "borrowed worker request".into(),
        false,
        Some(work.reservation.clone()),
    );
    requests.mark_queued(owner, target, request).unwrap();
    requests.present(target, request).unwrap();

    let cleanup = tokio::time::timeout(
        Duration::from_secs(1),
        work.cleanup(&fixture.environment, &fixture.kernel),
    )
    .await
    .unwrap();

    assert!(worker.terminal().get().is_none());
    assert_eq!(cleanup.requests.len(), 1);
    assert_eq!(cleanup.requests[0].request, request);
    assert_eq!(
        cleanup.requests[0].cancellation,
        Ok(crate::CancelRequestOutcome::Requested)
    );
    assert_eq!(
        cleanup.requests[0].target,
        Ok(crate::request::RequestCleanupState::CancellationRequested {
            reason: crate::CancellationReason::RequesterCancelled,
            presented: true,
        })
    );
    assert!(cleanup.uncertainty().is_some());
    assert!(matches!(
        fixture.deployments.try_recv(),
        Ok(LocalResidentDeployment::RequestCancellation { .. })
    ));
    requests
        .begin_cancellation_acknowledgement(target, request)
        .unwrap();
    requests.finish_cancellation_acknowledgement(request);

    fixture.cleanup(&work).await;

    assert_eq!(
        work.cleanup_observation().unwrap().requests[0].target,
        Ok(crate::request::RequestCleanupState::TargetClosed)
    );
    assert!(worker.terminal().get().is_none());
    fixture.finish().await;
}

#[tokio::test]
async fn invocation_cleanup_observes_request_closure_after_owned_workers_retire() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let work = InvocationWork::new(owner, reservation());
    let gate = Arc::new(ShutdownGate {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let requests = &fixture.environment.requests;
    let mut children = Vec::new();
    let mut pending = Vec::new();
    for index in 0..2 {
        let (send, receive) = tokio::sync::oneshot::channel();
        let mut behavior = Owner::new(send);
        behavior.requests = Some(requests.clone());
        if index == 1 {
            behavior.shutdown_gate = Some(gate.clone());
        }
        let child = fixture
            .kernel
            .spawn_worker_scoped(
                None,
                behavior,
                crate::WorkerLifetime::InvocationOwned,
                work.clone(),
            )
            .await
            .unwrap();
        let _context = receive.await.unwrap();
        let target = child.identity();
        let request = requests.reserve_for_operation(
            owner,
            target,
            "owned worker request".into(),
            false,
            Some(work.reservation.clone()),
        );
        requests.mark_queued(owner, target, request).unwrap();
        requests.present(target, request).unwrap();
        pending.push(request);
        children.push(child);
    }

    let mut cleanup = Box::pin(work.cleanup(&fixture.environment, &fixture.kernel));
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            result = &mut cleanup => panic!("worker retirement is still held: {result:?}"),
            permit = gate.entered.acquire() => permit.unwrap().forget(),
        }
    })
    .await
    .unwrap();
    assert!(work.cleanup_observation().is_none());
    assert_eq!(
        requests.request_cleanup_state(owner, pending[1]),
        Ok(crate::request::RequestCleanupState::CancellationRequested {
            reason: crate::CancellationReason::RequesterCancelled,
            presented: true,
        })
    );
    gate.release.add_permits(1);
    let cleanup = tokio::time::timeout(Duration::from_secs(1), cleanup)
        .await
        .expect("responsive owned workers finish cleanup");
    assert_eq!(cleanup.requests.len(), 2);
    assert_eq!(cleanup.workers.len(), 2);
    for request in &cleanup.requests {
        assert_eq!(
            request.cancellation,
            Ok(crate::CancelRequestOutcome::Requested)
        );
        assert_eq!(
            request.target,
            Ok(crate::request::RequestCleanupState::TargetClosed)
        );
    }
    assert_eq!(cleanup.uncertainty(), None);
    assert_eq!(work.cleanup_observation().unwrap().uncertainty(), None);
    for child in children {
        assert!(child.terminal().cleanup().unwrap().is_confirmed());
    }
    fixture.finish().await;
}

#[tokio::test]
async fn detached_invocation_request_survives_scope_cleanup_until_target_replies() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let target = ActorRef::first(crate::ActorId(owner.id.0 + 100));
    let work = InvocationWork::new(owner, reservation());
    let requests = &fixture.environment.requests;
    let request = requests.reserve_for_operation(
        owner,
        target,
        "detached request".into(),
        false,
        Some(work.reservation.clone()),
    );
    requests.mark_queued(owner, target, request).unwrap();
    requests.present(target, request).unwrap();
    work.detach_request(requests, owner, request).unwrap();

    fixture.cleanup(&work).await;

    assert!(work.cleanup_observation().unwrap().requests.is_empty());
    assert!(matches!(
        requests.observe_response(owner, request),
        Ok(ResponseObservation::Pending(_))
    ));
    requests.begin_reply(target, request).unwrap();
    requests.finish_reply(request, None);
    assert_eq!(
        requests.observe_response(owner, request),
        Ok(ResponseObservation::Ready)
    );
    fixture.finish().await;
}

#[tokio::test]
async fn command_detach_preserves_linked_source_probe_after_invocation_cleanup() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let (job, backend) = fixture.pending_command().await;
    let (probe, probe_backend) = fixture.pending_command().await;
    jobs.set_source_probe(&job, probe.clone()).unwrap();
    let work = InvocationWork::new(owner, reservation());
    work.register_command_with_jobs(&fixture.environment.commands, &job)
        .unwrap();
    work.register_command_with_jobs(&fixture.environment.commands, &probe)
        .unwrap();
    work.detach_command(jobs, owner, &job).unwrap();

    fixture.cleanup(&work).await;

    for (id, backend) in [(&job, backend), (&probe, probe_backend)] {
        assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
        assert!(!matches!(
            jobs.status(owner, id).await.unwrap(),
            CommandStatus::CommandFinished(_)
        ));
        backend.finish.add_permits(1);
        tokio::time::timeout(Duration::from_secs(1), jobs.finished(owner, id))
            .await
            .unwrap()
            .unwrap();
    }
    fixture.finish().await;
}

#[tokio::test(start_paused = true)]
async fn dropped_scoped_worker_start_retains_exact_kernel_and_host_cleanup_uncertainty() {
    let mut fixture = Fixture::start().await;
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    let gate = Arc::new(ShutdownGate {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let (send, receive) = tokio::sync::oneshot::channel();
    let mut spawning = Box::pin(fixture.kernel.spawn_worker_scoped(
        None,
        Owner {
            context: Some(send),
            startup_gate: Some(gate.clone()),
            shutdown_gate: None,
            requests: None,
        },
        crate::WorkerLifetime::InvocationOwned,
        work.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            result = &mut spawning => panic!("startup remains held: {result:?}"),
            permit = gate.entered.acquire() => permit.unwrap().forget(),
        }
    })
    .await
    .unwrap();
    let worker_context = receive.await.unwrap();
    let child = worker_context.identity();
    let worker = {
        let state = work.state.lock();
        assert!(state.pending_workers.is_empty());
        assert_eq!(state.workers.len(), 1);
        assert_eq!(state.workers[0].identity(), child);
        state.workers[0].clone()
    };
    work.close();
    drop(spawning);
    // Ractor cancels inline pre_start with the spawn future. A retained handle
    // permits observation; it cannot certify the interrupted startup's cleanup.
    assert_eq!(gate.calls.load(Ordering::Relaxed), 1);
    fixture
        .environment
        .release_tracked
        .store(true, Ordering::Release);
    let cleanup = tokio::time::timeout(
        crate::local_actor::SHUTDOWN_BUDGET + RELEASE_WAIT + Duration::from_secs(1),
        async {
            let (cleanup, ()) =
                tokio::join!(work.cleanup(&fixture.environment, &fixture.kernel), async {
                    let Some(LocalResidentDeployment::Retired { actor, terminal }) =
                        fixture.deployments.recv().await
                    else {
                        panic!("original worker retirement must precede host cleanup")
                    };
                    assert_eq!(actor, child);
                    assert_eq!(terminal.kind, ActorExitKind::Failed);
                    let Some(LocalResidentDeployment::ReleaseAwait(request)) =
                        fixture.deployments.recv().await
                    else {
                        panic!("exact worker cleanup must ask the host for its retained outcome")
                    };
                    assert_eq!(request.actor, child);
                    assert!(request.answer(ResourceRelease::Retained(
                        "startup resource release remains unconfirmed".into()
                    )));
                },);
            cleanup
        },
    )
    .await
    .unwrap();

    assert_eq!(cleanup.workers.len(), 1);
    assert_eq!(cleanup.workers[0].actor, child);
    assert!(!matches!(&cleanup.workers[0].kernel, Ok(outcome) if outcome.is_confirmed()));
    assert_eq!(
        cleanup.workers[0].host,
        Ok(ResourceRelease::Retained(
            "startup resource release remains unconfirmed".into()
        ))
    );
    assert!(cleanup.uncertainty().is_some());
    assert!(work.cleanup_observation().unwrap().uncertainty().is_some());
    assert!(work.owns_worker(worker.identity()));
    fixture.finish().await;
}

#[tokio::test]
async fn closed_invocation_refuses_real_scoped_spawn_before_worker_initialization() {
    let fixture = Fixture::start().await;
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    work.close();
    let gate = Arc::new(ShutdownGate {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let (send, receive) = tokio::sync::oneshot::channel();
    let result = fixture
        .kernel
        .spawn_worker_scoped(
            None,
            Owner {
                context: Some(send),
                startup_gate: Some(gate.clone()),
                shutdown_gate: None,
                requests: None,
            },
            crate::WorkerLifetime::InvocationOwned,
            work.clone(),
        )
        .await;

    assert!(result.is_err());
    assert_eq!(gate.calls.load(Ordering::Relaxed), 0);
    assert!(
        receive.await.is_err(),
        "worker start never receives its kernel context"
    );
    assert!(work.state.lock().pending_workers.is_empty());
    assert!(work.state.lock().workers.is_empty());
    fixture.cleanup(&work).await;
    fixture.finish().await;
}

#[tokio::test(start_paused = true)]
async fn interrupted_scope_retirement_admission_retries_original_terminal_once() {
    let mut fixture = Fixture::start().await;
    let (send, receive) = tokio::sync::oneshot::channel();
    let (worker, task) = crate::spawn_local_actor(None, Owner::new(send))
        .await
        .unwrap();
    let _worker_context = receive.await.unwrap();
    let child = worker.identity();
    let completed = ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "original successful worker outcome".into(),
        diagnostic: None,
    };
    let shutdown = worker
        .shutdown_with_cleanup(completed.clone())
        .await
        .unwrap();
    assert!(shutdown.cleanup.is_confirmed());
    task.await.unwrap();
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    work.register_worker(worker.clone()).unwrap();
    fixture
        .environment
        .release_tracked
        .store(true, Ordering::Release);
    let filler = LocalResidentDeployment::Retired {
        actor: fixture.actor.identity(),
        terminal: ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "channel filler".into(),
            diagnostic: None,
        },
    };
    for _ in 0..DEPLOYMENT_CHANNEL_CAPACITY {
        assert!(fixture
            .environment
            .deployments
            .try_send(filler.clone())
            .is_ok());
    }
    assert_eq!(fixture.environment.deployments.capacity(), 0);
    let mut first_cleanup = Box::pin(work.cleanup(&fixture.environment, &fixture.kernel));
    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut first_cleanup)
            .await
            .is_err(),
        "full retirement admission must wait"
    );
    assert!(!fixture.environment.retired.lock().contains(&child));
    assert!(work.cleanup_observation().is_none());
    assert!(work.owns_worker(child));
    assert_eq!(worker.terminal().get(), Some(completed.clone()));
    drop(first_cleanup);
    for _ in 0..DEPLOYMENT_CHANNEL_CAPACITY {
        let LocalResidentDeployment::Retired { actor, .. } =
            fixture.deployments.try_recv().unwrap()
        else {
            panic!("only filler events are queued")
        };
        assert_eq!(actor, fixture.actor.identity());
    }

    let cleanup = tokio::time::timeout(Duration::from_secs(1), async {
        let (cleanup, ()) =
            tokio::join!(work.cleanup(&fixture.environment, &fixture.kernel), async {
                let Some(LocalResidentDeployment::Retired { actor, terminal }) =
                    fixture.deployments.recv().await
                else {
                    panic!("retry must publish original retirement")
                };
                assert_eq!(actor, child);
                assert_eq!(terminal, completed);
                let Some(LocalResidentDeployment::ReleaseAwait(request)) =
                    fixture.deployments.recv().await
                else {
                    panic!("host cleanup follows retirement admission")
                };
                assert_eq!(request.actor, child);
                assert!(request.answer(ResourceRelease::Released));
            });
        cleanup
    })
    .await
    .unwrap();

    assert_eq!(cleanup.uncertainty(), None);
    assert_eq!(cleanup.workers.len(), 1);
    assert_eq!(cleanup.workers[0].kernel, Ok(shutdown.cleanup));
    assert_eq!(cleanup.workers[0].host, Ok(ResourceRelease::Released));
    assert!(fixture.environment.retired.lock().contains(&child));
    assert!(matches!(
        fixture.deployments.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    fixture.cleanup(&work).await;
    assert!(
        matches!(
            fixture.deployments.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "confirmed cleanup retry must not publish duplicate retirement or host cleanup"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn invocation_cleanup_rolls_back_unsubmitted_and_detached_original_reservations() {
    let mut fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let target = ActorRef::first(crate::ActorId(owner.id.0 + 100));
    let work = InvocationWork::new(owner, reservation());
    let sibling = InvocationWork::new(owner, reservation());
    let requests = &fixture.environment.requests;
    let reserve = |scope: &InvocationWork| {
        requests.reserve_for_operation(
            owner,
            target,
            "unsubmitted reservation".into(),
            false,
            Some(scope.reservation.clone()),
        )
    };
    let original = reserve(&work);
    let detached = reserve(&work);
    let sibling_request = reserve(&sibling);
    work.detach_request(requests, owner, detached).unwrap();
    let (watch, _) = requests
        .register_watch(owner, vec![original, detached])
        .unwrap();
    let subscription = requests.subscribe_watch(owner, watch).unwrap();

    fixture.cleanup(&work).await;

    for request in [original, detached] {
        assert_eq!(
            requests.observe_response(owner, request),
            Err(crate::ReplyError::Stale)
        );
    }
    assert!(matches!(
        requests.observe_response(owner, sibling_request),
        Ok(ResponseObservation::Pending(_))
    ));
    requests
        .mark_queued(owner, target, sibling_request)
        .expect("sibling reservation can still be published");
    assert!(work.cleanup_observation().unwrap().requests.is_empty());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), subscription.wait())
            .await
            .unwrap(),
        Ok(crate::request::WatchObservation::Unavailable {
            request: original,
            failure: crate::ResponseFailure::Released,
        })
    );
    let LocalResidentDeployment::WatchChanged { notification } =
        fixture.deployments.try_recv().unwrap()
    else {
        panic!("rollback publishes the original named watch's unavailable transition")
    };
    assert_eq!(notification.owner, owner);
    assert_eq!(notification.watch, watch);
    assert!(
        matches!(
            fixture.deployments.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "never-published requests must not emit cancellation events"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn interrupted_automatic_group_abort_retains_worker_for_invocation_cleanup() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let gate = Arc::new(ShutdownGate {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = fixture
        .kernel
        .spawn_worker(
            None,
            Owner {
                context: Some(send),
                startup_gate: None,
                shutdown_gate: Some(gate.clone()),
                requests: None,
            },
            crate::WorkerLifetime::ActorOwned,
        )
        .await
        .unwrap();
    let _worker_context = receive.await.unwrap();
    let child = worker.identity();
    let group = group_with_child(&fixture, child);
    let work = InvocationWork::new(owner, reservation());
    work.register_group(group).unwrap();
    let descriptor = ActorDescriptor::new(
        "automatic group abort fixture",
        crate::ActorPlacement {
            session: tidepool_repr::SessionId(1),
            resource_scope: RealmId::fresh(),
            lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
        },
    );
    let behavior = ResidentKernelBehavior::with_boot(
        descriptor,
        fixture.environment.clone(),
        ResidentBoot::Workbench,
        Vec::new(),
    );
    let mut abort = Box::pin(behavior.abort_incomplete_groups(
        &fixture.kernel,
        owner,
        None,
        "reply interrupted group admission",
        Some(work.as_ref()),
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = &mut abort => panic!("automatic abort must wait for the held worker shutdown"),
            permit = gate.entered.acquire() => permit.unwrap().forget(),
        }
    })
    .await
    .unwrap();
    assert!(work.owns_worker(child));
    assert!(matches!(
        fixture
            .environment
            .fork_groups
            .children_for_owner(group, owner),
        Err(crate::ForkGroupError::Unknown(_))
    ));
    drop(abort);
    gate.release.add_permits(1);

    fixture.cleanup(&work).await;

    let cleanup = work.cleanup_observation().unwrap();
    assert_eq!(cleanup.workers.len(), 1);
    assert_eq!(cleanup.workers[0].actor, child);
    assert_eq!(
        cleanup.workers[0].kernel,
        Ok(worker.terminal().cleanup().unwrap())
    );
    assert_eq!(gate.calls.load(Ordering::Relaxed), 1);
    drop(behavior);
    fixture.finish().await;
}

#[tokio::test(start_paused = true)]
async fn interrupted_unsubmitted_rollback_notice_retries_original_watch_transition_once() {
    let mut fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let target = ActorRef::first(crate::ActorId(owner.id.0 + 100));
    let work = InvocationWork::new(owner, reservation());
    let requests = &fixture.environment.requests;
    let request = requests.reserve_for_operation(
        owner,
        target,
        "unsubmitted request".into(),
        false,
        Some(work.reservation.clone()),
    );
    let (watch, _) = requests.register_watch(owner, vec![request]).unwrap();
    let subscription = requests.subscribe_watch(owner, watch).unwrap();
    let filler = LocalResidentDeployment::Retired {
        actor: owner,
        terminal: ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "channel filler".into(),
            diagnostic: None,
        },
    };
    for _ in 0..DEPLOYMENT_CHANNEL_CAPACITY {
        assert!(fixture
            .environment
            .deployments
            .try_send(filler.clone())
            .is_ok());
    }
    let mut first_cleanup = Box::pin(work.cleanup(&fixture.environment, &fixture.kernel));
    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut first_cleanup)
            .await
            .is_err(),
        "full rollback notice admission must wait"
    );
    assert_eq!(
        requests.observe_response(owner, request),
        Err(crate::ReplyError::Stale)
    );
    let pending = {
        let state = work.state.lock();
        assert_eq!(state.pending_watch_notifications.len(), 1);
        state.pending_watch_notifications[0].clone()
    };
    assert_eq!(pending.owner, owner);
    assert_eq!(pending.watch, watch);
    assert_eq!(
        pending.transition,
        crate::request::WatchTransition::Unavailable {
            request,
            failure: crate::ResponseFailure::Released,
        }
    );
    assert!(work.cleanup_observation().is_none());
    assert_eq!(
        subscription.wait().await,
        Ok(crate::request::WatchObservation::Unavailable {
            request,
            failure: crate::ResponseFailure::Released,
        })
    );
    drop(first_cleanup);
    for _ in 0..DEPLOYMENT_CHANNEL_CAPACITY {
        let LocalResidentDeployment::Retired { actor, .. } =
            fixture.deployments.try_recv().unwrap()
        else {
            panic!("only filler events were admitted")
        };
        assert_eq!(actor, owner);
    }

    fixture.cleanup(&work).await;

    let LocalResidentDeployment::WatchChanged { notification } =
        fixture.deployments.try_recv().unwrap()
    else {
        panic!("retry must deliver the original rollback notice")
    };
    assert_eq!(
        notification, pending,
        "retry retains exact sequence, watermark, timestamp and correlation"
    );
    assert!(work.state.lock().pending_watch_notifications.is_empty());
    assert!(work.cleanup_observation().unwrap().requests.is_empty());
    assert!(
        matches!(
            fixture.deployments.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "no duplicate watch notice or never-published request cancellation is queued"
    );
    fixture.cleanup(&work).await;
    assert!(matches!(
        fixture.deployments.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    fixture.finish().await;
}
