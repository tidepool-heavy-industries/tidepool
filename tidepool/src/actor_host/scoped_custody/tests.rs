use super::*;
use crate::actor_host::*;
use tidepool_actor::ForkWorkspaceCustody;
use tidepool_node::{
    run_process_supervisor, LaunchReservation, ProcessInvocation, ProcessMountBoundary,
    ProcessSupervisorClient, ProcessSupervisorManifest,
};

struct Fixture {
    _repo: tidepool_worktree::testing::TestRepo,
    _runtime: tempfile::TempDir,
    tree: WorktreeHandle,
    custody: Arc<ActorWorkspaceCustody>,
}

#[test]
fn replacement_transfers_unlaunched_workspace_without_old_guard_release() {
    let fixture = Fixture::new(901);
    let successor = ActorRef::first(tidepool_actor::ActorId(902));
    let bindings = fixture.custody.bindings.clone();
    let principal = WorktreePrincipal::exact_actor("scope-test", 902, 1);
    let transferred = fixture.custody.transfer_to(successor).unwrap();
    assert!(fixture.custody.binding.lock().is_none());
    assert!(fixture.custody.transfer_to(successor).is_err());
    assert_eq!(
        bindings.lock().active_for_agent(&principal),
        Some(fixture.tree.id())
    );
    drop(fixture.custody);
    assert_eq!(
        bindings.lock().active_for_agent(&principal),
        Some(fixture.tree.id())
    );
    transferred.actor_stopped(&ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "replacement finished".into(),
    });
    drop(transferred);
    assert!(bindings.lock().current(fixture.tree.id()).is_none());
}

#[test]
fn replacement_can_restore_original_workspace_owner_before_cutover() {
    let fixture = Fixture::new(905);
    let original = ActorRef::first(tidepool_actor::ActorId(905));
    let successor = ActorRef::first(tidepool_actor::ActorId(906));
    let bindings = fixture.custody.bindings.clone();
    let transferred = fixture.custody.transfer_to(successor).unwrap();
    let restored = transferred.transfer_to(original).unwrap();
    drop(transferred);
    drop(fixture.custody);
    assert_eq!(
        bindings.lock().current(fixture.tree.id()).unwrap().agent(),
        &WorktreePrincipal::exact_actor("scope-test", 905, 1)
    );
    assert!(bindings
        .lock()
        .active_for_agent(&WorktreePrincipal::exact_actor("scope-test", 906, 1))
        .is_none());
    restored.actor_stopped(&ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "original owner finished".into(),
    });
    drop(restored);
    assert!(bindings.lock().current(fixture.tree.id()).is_none());
}

#[test]
fn replacement_cannot_transfer_workspace_with_uncertain_process_custody() {
    let fixture = Fixture::new(903);
    fixture.custody.process_may_exist();
    let successor = ActorRef::first(tidepool_actor::ActorId(904));
    assert!(fixture.custody.transfer_to(successor).is_err());
    assert!(fixture.custody.binding.lock().is_some());
    assert_eq!(
        fixture
            .custody
            .bindings
            .lock()
            .current(fixture.tree.id())
            .unwrap()
            .agent(),
        &WorktreePrincipal::exact_actor("scope-test", 903, 1)
    );
}

impl Fixture {
    fn new(id: u64) -> Self {
        let repo = tidepool_worktree::testing::TestRepo::init().unwrap();
        repo.writer()
            .commit_file("README.md", "seed", "seed")
            .unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let (manager, mut bindings) =
            actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
        let tree = manager
            .create(&tidepool_worktree::WorktreeSpec::from_current_repository(
                "scope",
            ))
            .unwrap();
        let actor = ActorRef::first(tidepool_actor::ActorId(id));
        let binding = bindings
            .bind(
                tree.id(),
                &WorktreePrincipal::exact_actor("scope-test", id, 1),
                0,
            )
            .unwrap();
        Self {
            _repo: repo,
            _runtime: runtime,
            tree,
            custody: Arc::new(ActorWorkspaceCustody {
                runtime: "scope-test".into(),
                workspace: None,
                inheritance_notice: None,
                bindings: Arc::new(Mutex::new(bindings)),
                binding: Mutex::new(Some(binding)),
                actor,
                state: Arc::new(Mutex::new(CustodyState::default())),
            }),
        }
    }

    fn prepared(&self, executable: impl Into<PathBuf>) -> LaunchReservation {
        ProcessMountBoundary::new(
            self.tree.cwd(),
            [self.tree.cwd().to_owned()],
            [self.tree.cwd().to_owned()],
        )
        .unwrap()
        .prepare_service_scope(
            executable.into(),
            ProcessInvocation {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "echo forbidden > started".into()],
            },
        )
        .unwrap()
    }

    fn register(&self, owners: &InteractiveOwners) {
        owners.lock().insert(
            self.custody.actor,
            InteractiveApplicationOwner {
                supervisor: None,
                creator_workspace: None,
                cancel: None,
                native_retirement: Default::default(),
                pane: Arc::new(Mutex::new(None)),
                fork_gate: None,
                custody: Some(self.custody.clone()),
                scoped_retention: None,
                hosted: Arc::new(Mutex::new(None)),
                launch: HostLaunchState::Pending,
                pending_activations: Vec::new(),
                terminal: None,
                retirement: Arc::new(Mutex::new(None)),
            },
        );
    }

    fn claim(
        &self,
        owners: &InteractiveOwners,
        actor: ActorRef,
    ) -> Result<Arc<Mutex<ScopedProcessSlot>>, ScopedClaimError> {
        owners
            .lock()
            .get_mut(&self.custody.actor)
            .unwrap()
            .reserve_scope(ActorWorkspaceRequest::Worktree("bound"), actor)
    }

    fn spawn(&self, owners: &InteractiveOwners) -> Arc<Mutex<ScopedProcessSlot>> {
        let slot = self.claim(owners, self.custody.actor).unwrap();
        spawn_into(
            slot.clone(),
            self.prepared(bwrap()),
            ServiceEnvironment::default(),
            File::create(self.tree.cwd().join("scope.log")).unwrap(),
        )
        .unwrap();
        slot
    }

    fn retained(&self) {
        assert!(self
            .custody
            .bindings
            .lock()
            .current(self.tree.id())
            .is_some());
        assert!(
            !self.tree.cwd().join("started").exists(),
            "payload must stay gated"
        );
    }
}

fn bwrap() -> PathBuf {
    std::env::var_os("SERVICE_SCOPE_BWRAP")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("/nix/store/dqzmpjz70l4lzg7lmc3x8wih74nh5bpc-bubblewrap-0.11.0/bin/bwrap")
        })
}
fn deadline() -> Instant {
    Instant::now() + std::time::Duration::from_secs(10)
}
fn cancelled() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "test retirement".into(),
    }
}

fn owners() -> InteractiveOwners {
    Arc::new(Mutex::new(HashMap::new()))
}

#[test]
fn production_supervisor_row_drives_real_helper_and_finalizes_exact_launch() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let boundary = ProcessMountBoundary::new(
        directory.path(),
        [directory.path().to_owned()],
        [directory.path().to_owned()],
    )
    .unwrap();
    let manifest = ProcessSupervisorManifest::new(
        "production-row".into(),
        "p".repeat(64),
        "r".repeat(64),
        directory.path().to_owned(),
        bwrap(),
        boundary,
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo started > started; exec sleep 30".into()],
        },
        ServiceEnvironment::default(),
    )
    .unwrap();
    let socket = manifest.socket_path();
    let manifest_path = manifest.write_new().unwrap();
    let slot = Arc::new(Mutex::new(ScopedProcessSlot::Reserved));
    stage_supervisor(
        &slot,
        SupervisorRecoveryKey::new(socket.clone(), "production-row".into(), "r".repeat(64)),
    )
    .unwrap();

    std::thread::scope(|scope| {
        let server = scope.spawn(|| run_process_supervisor(&manifest_path).unwrap());
        let socket_deadline = deadline();
        while !socket.exists() {
            assert!(
                Instant::now() < socket_deadline,
                "supervisor socket startup"
            );
            std::thread::yield_now();
        }
        let (client, observation) = ProcessSupervisorClient::pair(
            socket,
            "production-row".into(),
            "p".repeat(64),
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        install_supervisor(&slot, client, observation).unwrap();
        assert_eq!(
            prepare_supervisor_slot(&slot, deadline()).unwrap(),
            ScopedProcessObservation::Blocked
        );
        assert_eq!(
            pin_supervisor_slot(&slot, deadline()).unwrap(),
            ScopedProcessObservation::Pinned
        );
        assert!(!directory.path().join("started").exists());
        assert_eq!(
            release_supervisor_slot(&slot, deadline()).unwrap(),
            ScopedProcessObservation::Released
        );
        let started_deadline = deadline();
        while !directory.path().join("started").exists() {
            assert!(
                Instant::now() < started_deadline,
                "released payload startup"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            stop_retained_slot(&slot, deadline()).unwrap(),
            ScopedProcessObservation::ProcessStopped
        );
        assert!(matches!(*slot.lock(), ScopedProcessSlot::Finalized));
        server.join().unwrap();
    });
}

#[test]
fn production_supervisor_row_finalizes_definitive_pre_spawn_stop() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let boundary = ProcessMountBoundary::new(
        directory.path(),
        [directory.path().to_owned()],
        [directory.path().to_owned()],
    )
    .unwrap();
    let manifest = ProcessSupervisorManifest::new(
        "pre-spawn-stop".into(),
        "p".repeat(64),
        "r".repeat(64),
        directory.path().to_owned(),
        bwrap(),
        boundary,
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo forbidden > started".into()],
        },
        ServiceEnvironment::default(),
    )
    .unwrap();
    let socket = manifest.socket_path();
    let manifest_path = manifest.write_new().unwrap();
    let slot = Arc::new(Mutex::new(ScopedProcessSlot::Reserved));
    stage_supervisor(
        &slot,
        SupervisorRecoveryKey::new(socket.clone(), "pre-spawn-stop".into(), "r".repeat(64)),
    )
    .unwrap();

    std::thread::scope(|scope| {
        let server = scope.spawn(|| run_process_supervisor(&manifest_path).unwrap());
        let limit = deadline();
        while !socket.exists() {
            assert!(Instant::now() < limit, "supervisor socket startup");
            std::thread::yield_now();
        }
        let (client, observed) = ProcessSupervisorClient::pair(
            socket,
            "pre-spawn-stop".into(),
            "p".repeat(64),
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        install_supervisor(&slot, client, observed).unwrap();
        assert_eq!(
            stop_retained_slot(&slot, deadline()).unwrap(),
            ScopedProcessObservation::ProcessStopped
        );
        assert!(matches!(*slot.lock(), ScopedProcessSlot::Finalized));
        server.join().unwrap();
    });
    assert!(!directory.path().join("started").exists());
}

#[test]
fn scoped_custody_exact_claim_and_pre_spawn_failure() {
    let mut fixture = Fixture::new(1);
    let map = owners();
    fixture.register(&map);
    assert!(matches!(
        fixture.claim(&map, ActorRef::first(tidepool_actor::ActorId(2))),
        Err(ScopedClaimError::WrongActor)
    ));
    let mut next = fixture.custody.actor;
    next.incarnation = tidepool_actor::Incarnation(2);
    assert!(matches!(
        fixture.claim(&map, next),
        Err(ScopedClaimError::WrongActor)
    ));
    let slot = fixture.claim(&map, fixture.custody.actor).unwrap();
    spawn_into(
        slot.clone(),
        fixture.prepared("/does/not/exist"),
        ServiceEnvironment::default(),
        File::create(fixture.tree.cwd().join("scope.log")).unwrap(),
    )
    .unwrap();
    assert!(
        matches!(&*slot.lock(), ScopedProcessSlot::NotSpawned(ServiceScopeError::NotSpawned(error))
        if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(matches!(
        fixture.claim(&map, fixture.custody.actor),
        Err(ScopedClaimError::AlreadyClaimed)
    ));
    assert!(map
        .lock()
        .get_mut(&fixture.custody.actor)
        .unwrap()
        .scoped_retention
        .as_mut()
        .unwrap()
        .observe_not_spawned());
    map.lock().remove(&fixture.custody.actor); // Definitive pre-spawn rollback only.
    drop(slot);
    let lease = Arc::get_mut(&mut fixture.custody)
        .unwrap()
        .binding
        .get_mut()
        .take()
        .unwrap();
    lease.release(&mut fixture.custody.bindings.lock()).unwrap();
    let new_lease = fixture
        .custody
        .bindings
        .lock()
        .bind(
            fixture.tree.id(),
            &WorktreePrincipal::exact_actor("scope-test", 1, 1),
            1,
        )
        .unwrap();
    fixture.register(&map);
    assert!(matches!(
        fixture.claim(&map, fixture.custody.actor),
        Err(ScopedClaimError::MissingLease)
    ));
    new_lease
        .release(&mut fixture.custody.bindings.lock())
        .unwrap();
}

#[tokio::test]
async fn scoped_custody_lost_spawn_and_retirement_result_remain_addressable() {
    let fixture = Fixture::new(1);
    let map = owners();
    fixture.register(&map);
    let slot = fixture.claim(&map, fixture.custody.actor).unwrap();
    let prepared = fixture.prepared(bwrap());
    let output = File::create(fixture.tree.cwd().join("scope.log")).unwrap();
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    drop(receive); // BEFORE spawn, and therefore before any pin.
    let worker = tokio::task::spawn_blocking(move || {
        spawn_into(slot, prepared, ServiceEnvironment::default(), output).unwrap();
        assert!(send.send(()).is_err()); // Stored in row before lost notification.
    });
    worker.await.unwrap();
    fixture.retained();
    let slot = {
        let mut rows = map.lock();
        let row = rows.get_mut(&fixture.custody.actor).unwrap();
        let retained = row.scoped_retention.as_mut().unwrap();
        retained.pin(deadline()).unwrap();
        retained.slot.clone()
    };
    let (send, receive) = tokio::sync::oneshot::channel();
    drop(receive);
    tokio::task::spawn_blocking(move || {
        let status = stop_slot(&slot, deadline()).unwrap();
        assert!(send.send(status).is_err());
    })
    .await
    .unwrap();
    let retained_slot = {
        map.lock()
            .get(&fixture.custody.actor)
            .unwrap()
            .scoped_retention
            .as_ref()
            .map(|retention| retention.slot.clone())
    };
    let exact_outcome = retire_scoped_process(retained_slot, NativeRetirement::Terminate)
        .await
        .unwrap();
    assert_eq!(exact_outcome, CleanupComponentOutcome::Completed);
    {
        let mut rows = map.lock();
        let row = rows.get_mut(&fixture.custody.actor).unwrap();
        assert!(matches!(
            row.scoped_retention
                .as_mut()
                .unwrap()
                .stop(deadline())
                .unwrap(),
            ScopedCleanupObservation::ProcessStoppedActorActive(_)
        ));
        row.retired(cancelled());
        row.retired(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "duplicate".into(),
        });
        assert_eq!(row.terminal, Some(cancelled()));
        match row
            .scoped_retention
            .as_mut()
            .unwrap()
            .stop(deadline())
            .unwrap()
        {
            ScopedCleanupObservation::ProcessStoppedHostWorkPending { terminal, status } => {
                assert_eq!(terminal, cancelled());
                let retention = row.scoped_retention.as_ref().unwrap();
                let slot = retention.slot.lock();
                let ScopedProcessSlot::Owned(scope) = &*slot else {
                    panic!("retained exact scope")
                };
                assert!(
                    matches!(scope.observation().unwrap(), tidepool_node::ScopeObservation::ProcessStopped(retained)
                    if retained.monitor_status() == status.monitor_status())
                );
            }
            _ => panic!("terminal observed"),
        }
    }
    let (resume, blocked) = tokio::sync::oneshot::channel();
    let mut task = tokio::spawn(async move {
        blocked.await.unwrap();
        Ok(())
    });
    assert!(await_applications(&mut task, Duration::ZERO).await.is_err());
    assert!(
        !task.is_finished(),
        "timeout must not abort the fleet owner"
    );
    let mut error =
        handoff_application_owners(map, task, Err(runtime_error("host work pending")), Ok(()))
            .unwrap_err();
    let teardown =
        RetainedInteractiveFleet::from_error(error.as_mut()).expect("actual host carrier");
    resume.send(()).unwrap();
    teardown.unfinished.take().unwrap().await.unwrap().unwrap();
    fixture.retained();
    assert!(matches!(
        *teardown
            .owners
            .lock()
            .get(&fixture.custody.actor)
            .unwrap()
            .scoped_retention
            .as_ref()
            .unwrap()
            .slot
            .lock(),
        ScopedProcessSlot::Owned(_)
    ));
    drop(error); // Fixture namespace was explicitly stopped through exact slot.
    assert_eq!(
        Arc::strong_count(&fixture.custody),
        1,
        "no backref, cycle or leak"
    );
    fixture.retained(); // No successful binding settlement is claimed.
}

#[tokio::test]
async fn scoped_retirement_preserve_keeps_exact_process_and_reports_uncertainty() {
    let fixture = Fixture::new(1);
    let map = owners();
    fixture.register(&map);
    let slot = fixture.spawn(&map);
    map.lock()
        .get_mut(&fixture.custody.actor)
        .unwrap()
        .scoped_retention
        .as_mut()
        .unwrap()
        .pin(deadline())
        .unwrap();

    let outcome = retire_scoped_process(Some(slot), NativeRetirement::Preserve)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        CleanupComponentOutcome::Failed { detail }
            if detail.contains("intentionally preserved")
    ));
    fixture.retained();

    map.lock()
        .get_mut(&fixture.custody.actor)
        .unwrap()
        .scoped_retention
        .as_mut()
        .unwrap()
        .stop(deadline())
        .unwrap();
}

#[test]
fn scoped_custody_concurrent_claim_sibling_timeout_and_legacy_fence() {
    let first = Fixture::new(1);
    let second = Fixture::new(2);
    let map = owners();
    first.register(&map);
    second.register(&map);
    let barrier = std::sync::Barrier::new(2);
    let claim = || {
        barrier.wait();
        first.claim(&map, first.custody.actor)
    };
    let slot = std::thread::scope(|scope| {
        let a = scope.spawn(claim);
        let b = scope.spawn(claim);
        match (a.join().unwrap(), b.join().unwrap()) {
            (Ok(slot), Err(ScopedClaimError::AlreadyClaimed))
            | (Err(ScopedClaimError::AlreadyClaimed), Ok(slot)) => slot,
            _ => panic!("one exact claim"),
        }
    });
    assert!(matches!(
        map.lock()
            .get_mut(&second.custody.actor)
            .unwrap()
            .reserve_scope(
                ActorWorkspaceRequest::Worktree("bound"),
                first.custody.actor
            ),
        Err(ScopedClaimError::WrongActor)
    ));
    spawn_into(
        slot.clone(),
        first.prepared(bwrap()),
        ServiceEnvironment::default(),
        File::create(first.tree.cwd().join("scope.log")).unwrap(),
    )
    .unwrap();
    assert!(spawn_into(
        slot.clone(),
        first.prepared(bwrap()),
        ServiceEnvironment::default(),
        File::create(first.tree.cwd().join("duplicate.log")).unwrap()
    )
    .is_err());
    let sibling = second.spawn(&map);
    {
        let mut rows = map.lock();
        let owner = rows
            .get_mut(&first.custody.actor)
            .unwrap()
            .scoped_retention
            .as_mut()
            .unwrap();
        assert!(owner.stop(Instant::now()).is_err());
        owner.pin(deadline()).unwrap();
        assert!(owner
            .stop(Instant::now() - std::time::Duration::from_secs(1))
            .is_err());
        first.retained();
        owner.stop(deadline()).unwrap();
        let owner = rows
            .get_mut(&second.custody.actor)
            .unwrap()
            .scoped_retention
            .as_mut()
            .unwrap();
        owner.pin(deadline()).unwrap();
        let copied = match owner.stop(deadline()).unwrap() {
            ScopedCleanupObservation::ProcessStoppedActorActive(status) => status,
            _ => panic!("active"),
        };
        let status = copied;
        assert_eq!(status.monitor_status(), copied.monitor_status());
    }
    first.retained();
    second.retained();
    drop(slot);
    drop(sibling);
    drop(map);
    assert_eq!(Arc::strong_count(&first.custody), 1);

    let legacy = Fixture::new(3);
    legacy.custody.process_may_exist();
    let map = owners();
    legacy.register(&map);
    assert!(matches!(
        legacy.claim(&map, legacy.custody.actor),
        Err(ScopedClaimError::AlreadyClaimed)
    ));
    legacy.custody.actor_stopped(&cancelled());
    let table = legacy.custody.bindings.clone();
    let id = legacy.tree.id().clone();
    drop(map);
    drop(legacy.custody);
    assert!(table.lock().current(&id).is_some());
}

#[test]
fn scoped_custody_pin_error_retains_owner() {
    let fixture = Fixture::new(1);
    let map = owners();
    fixture.register(&map);
    let slot = fixture.claim(&map, fixture.custody.actor).unwrap();
    spawn_into(
        slot.clone(),
        fixture.prepared("/run/current-system/sw/bin/false"),
        ServiceEnvironment::default(),
        File::create(fixture.tree.cwd().join("scope.log")).unwrap(),
    )
    .unwrap();
    {
        let mut rows = map.lock();
        let retained = rows
            .get_mut(&fixture.custody.actor)
            .unwrap()
            .scoped_retention
            .as_mut()
            .unwrap();
        assert!(retained.pin(deadline()).is_err());
        assert!(retained.stop(deadline()).is_err());
        assert!(matches!(*retained.slot.lock(), ScopedProcessSlot::Owned(_)));
    }
    fixture.retained();
    drop(slot);
    drop(map);
    assert_eq!(Arc::strong_count(&fixture.custody), 1);
}

#[tokio::test]
async fn scoped_custody_production_handoff_recovers_completed_and_timed_out_fleets() {
    for completed in [true, false] {
        let fixture = Fixture::new(1);
        let map = owners();
        fixture.register(&map);
        let slot = fixture.claim(&map, fixture.custody.actor).unwrap();
        let prepared = fixture.prepared(bwrap());
        let output = File::create(fixture.tree.cwd().join("scope.log")).unwrap();
        let (notice, receive) = tokio::sync::oneshot::channel::<()>();
        drop(receive); // No pin has occurred and the receiver is already gone.
        tokio::task::spawn_blocking(move || {
            spawn_into(slot, prepared, ServiceEnvironment::default(), output).unwrap();
            assert!(notice.send(()).is_err());
        })
        .await
        .unwrap();
        map.lock()
            .get_mut(&fixture.custody.actor)
            .unwrap()
            .retired(cancelled());

        let (resume, blocked) = tokio::sync::oneshot::channel();
        let mut task = tokio::spawn(async move {
            blocked.await.unwrap();
            Ok(())
        });
        let (cleanup, resume) = if completed {
            resume.send(()).unwrap();
            (
                await_applications(&mut task, Duration::from_secs(10)).await,
                None,
            )
        } else {
            let cleanup = await_applications(&mut task, Duration::ZERO).await;
            assert!(cleanup.is_err());
            (cleanup, Some(resume))
        };
        // This is run's actual predicate/construction, not a hand-built owner.
        let mut error: Box<dyn std::error::Error> =
            handoff_application_owners(map, task, cleanup, Ok(())).unwrap_err();
        let carrier = RetainedInteractiveFleet::from_error(error.as_mut()).unwrap();
        assert_eq!(carrier.unfinished.is_none(), completed);
        let mut wrong = fixture.custody.actor;
        wrong.incarnation = tidepool_actor::Incarnation(2);
        assert!(matches!(
            carrier.recover_process(wrong, RetainedProcessOperation::Observe, deadline()),
            Err(RetainedProcessError::NoScopedActor)
        ));
        assert!(matches!(
            carrier.recover_process(
                fixture.custody.actor,
                RetainedProcessOperation::Pin,
                Instant::now()
            ),
            Err(RetainedProcessError::Deadline)
        ));
        let observed = carrier
            .recover_process(
                fixture.custody.actor,
                RetainedProcessOperation::Observe,
                deadline(),
            )
            .unwrap();
        assert_eq!(observed.actor, fixture.custody.actor);
        assert_eq!(observed.actor_terminal, Some(cancelled()));
        assert!(matches!(observed.process, RetainedProcessState::Blocked));
        assert!(matches!(
            carrier
                .recover_process(
                    fixture.custody.actor,
                    RetainedProcessOperation::Pin,
                    deadline()
                )
                .unwrap()
                .process,
            RetainedProcessState::Pinned
        ));
        for _ in 0..2 {
            let observed = carrier
                .recover_process(
                    fixture.custody.actor,
                    RetainedProcessOperation::Stop,
                    deadline(),
                )
                .unwrap();
            assert_eq!(observed.actor_terminal, Some(cancelled()));
            assert!(matches!(
                observed.process,
                RetainedProcessState::ProcessStopped(_)
            ));
        }
        fixture.retained(); // Stopped process is not host-work settlement.
        if let Some(resume) = resume {
            resume.send(()).unwrap();
            carrier.unfinished.take().unwrap().await.unwrap().unwrap();
        }
        drop(error);
        assert_eq!(
            Arc::strong_count(&fixture.custody),
            1,
            "no carrier/slot cycle"
        );
        fixture.retained();
    }
}

#[derive(Debug)]
struct HandoffError(&'static str);
impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for HandoffError {}

#[tokio::test]
async fn scoped_custody_handoff_preserves_unretained_results() {
    for (cleanup_failed, run_failed) in [(false, false), (false, true), (true, false), (true, true)]
    {
        let mut task = tokio::spawn(async { Ok(()) });
        await_applications(&mut task, Duration::from_secs(10))
            .await
            .unwrap();
        let cleanup = if cleanup_failed {
            Err(Box::new(HandoffError("cleanup")) as Box<dyn std::error::Error>)
        } else {
            Ok(())
        };
        let result = if run_failed {
            Err(Box::new(HandoffError("run")) as Box<dyn std::error::Error>)
        } else {
            Ok(())
        };
        let result = handoff_application_owners(owners(), task, cleanup, result);
        if cleanup_failed || run_failed {
            let error = result.unwrap_err();
            // Preserve concrete error identity, not a String replacement.
            assert_eq!(
                error.downcast_ref::<HandoffError>().unwrap().0,
                if cleanup_failed { "cleanup" } else { "run" }
            );
        } else {
            result.unwrap();
        }
    }
}
