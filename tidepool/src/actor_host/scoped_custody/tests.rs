use super::*;
use crate::actor_host::*;
use tidepool_actor::ForkWorkspaceCustody;
use tidepool_node::{ProcessInvocation, ProcessMountBoundary};

struct Fixture {
    _repo: tidepool_worktree::testing::TestRepo,
    _runtime: tempfile::TempDir,
    tree: WorktreeHandle,
    custody: Arc<ActorWorkspaceCustody>,
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
                bindings: Arc::new(Mutex::new(bindings)),
                binding: Some(binding),
                actor,
                state: Mutex::new(CustodyState::default()),
            }),
        }
    }

    fn prepared(&self, executable: &str) -> PreparedServiceScope {
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

    fn spawn_for(
        &self,
        actor: ActorRef,
        executable: &str,
    ) -> Result<ScopedCustodyOwner, ScopedSpawnError> {
        ScopedCustodyOwner::spawn(
            self.custody.clone(),
            actor,
            self.prepared(executable),
            ServiceEnvironment::default(),
            File::create(self.tree.cwd().join("scope.log")).unwrap(),
        )
    }

    fn spawn(&self) -> ScopedCustodyOwner {
        self.spawn_for(self.custody.actor, bwrap()).unwrap()
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

fn bwrap() -> &'static str {
    "/nix/store/dqzmpjz70l4lzg7lmc3x8wih74nh5bpc-bubblewrap-0.11.0/bin/bwrap"
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

#[test]
fn scoped_custody_exact_claim_and_pre_spawn_failure() {
    let mut fixture = Fixture::new(1);
    assert!(matches!(
        fixture.spawn_for(ActorRef::first(tidepool_actor::ActorId(2)), bwrap()),
        Err(ScopedSpawnError::WrongActor)
    ));
    let mut other_incarnation = fixture.custody.actor;
    other_incarnation.incarnation = tidepool_actor::Incarnation(2);
    assert!(matches!(
        fixture.spawn_for(other_incarnation, bwrap()),
        Err(ScopedSpawnError::WrongActor)
    ));
    assert!(matches!(
        fixture.spawn_for(fixture.custody.actor, "/does/not/exist"),
        Err(ScopedSpawnError::PreSpawn(_))
    ));
    assert!(matches!(
        fixture.spawn_for(fixture.custody.actor, bwrap()),
        Err(ScopedSpawnError::AlreadyClaimed)
    ));
    fixture.retained();

    // No copied generation is an admission token: removing the exact old lease
    // and rebinding cannot turn the old custody object into a claim for the new row.
    let lease = Arc::get_mut(&mut fixture.custody)
        .unwrap()
        .binding
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
    assert!(matches!(
        fixture.spawn_for(fixture.custody.actor, bwrap()),
        Err(ScopedSpawnError::MissingLease)
    ));
    new_lease
        .release(&mut fixture.custody.bindings.lock())
        .unwrap();

    let fixture = Fixture::new(3);
    assert!(matches!(
        fixture.spawn_for(fixture.custody.actor, "/does/not/exist"),
        Err(ScopedSpawnError::PreSpawn(_))
    ));
    let table = fixture.custody.bindings.clone();
    let id = fixture.tree.id().clone();
    drop(fixture.custody);
    assert!(
        table.lock().current(&id).is_none(),
        "true pre-spawn failure must not create process custody"
    );
    let stopped = Fixture::new(4);
    stopped.custody.actor_stopped(&cancelled());
    assert!(matches!(
        stopped.spawn_for(stopped.custody.actor, bwrap()),
        Err(ScopedSpawnError::ActorStopped)
    ));
}

#[test]
fn scoped_custody_own_scope_actor_and_host_retention() {
    let first = Fixture::new(1);
    let second = Fixture::new(2);
    let barrier = std::sync::Barrier::new(2);
    let claim = || {
        barrier.wait();
        first.spawn_for(first.custody.actor, bwrap())
    };
    let mut owner = std::thread::scope(|threads| {
        let a = threads.spawn(&claim);
        let b = threads.spawn(&claim);
        match (a.join().unwrap(), b.join().unwrap()) {
            (Ok(owner), Err(ScopedSpawnError::AlreadyClaimed))
            | (Err(ScopedSpawnError::AlreadyClaimed), Ok(owner)) => owner,
            _ => panic!("exactly one concurrent claim must own its spawn"),
        }
    });
    let mut sibling = second.spawn();
    assert!(matches!(
        first.spawn_for(first.custody.actor, bwrap()),
        Err(ScopedSpawnError::AlreadyClaimed)
    ));
    owner.pin(deadline()).unwrap();
    sibling.pin(deadline()).unwrap();
    let status = match sibling.stop(deadline()).unwrap() {
        ScopedCleanupObservation::ProcessStoppedActorActive(status) => status,
        _ => panic!("actor is still active"),
    };
    // Copyable status has no path into another owner's cleanup/settlement API.
    let copied = status;
    assert_eq!(copied.monitor_status(), status.monitor_status());
    first.retained();
    second.retained();
    let first_status = owner.stop(deadline()).unwrap();
    assert!(matches!(
        first_status,
        ScopedCleanupObservation::ProcessStoppedActorActive(_)
    ));
    first.custody.actor_stopped(&cancelled());
    first.custody.actor_stopped(&ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "duplicate".into(),
    });
    match owner.stop(deadline()).unwrap() {
        ScopedCleanupObservation::ProcessStoppedHostWorkPending { terminal, status } => {
            assert_eq!(terminal, cancelled());
            assert!(!status.monitor_status().success());
        }
        _ => panic!("observed actor terminal is not actor-active"),
    }
    drop(owner);
    drop(sibling);
    first.retained();
    second.retained();
    assert!(
        Arc::strong_count(&first.custody) > 1,
        "host-pending guard must retain its custody owner"
    );
}

#[test]
fn scoped_custody_pin_timeout_lost_result_and_legacy_fence() {
    let fixture = Fixture::new(1);
    let mut owner = fixture.spawn();
    // Expired deadline guarantees no wait-based positive cleanup evidence.
    assert!(
        owner.stop(Instant::now()).is_err(),
        "unvalidated init is not cleanup proof"
    );
    fixture.retained();
    owner.pin(deadline()).unwrap();
    assert!(owner
        .stop(Instant::now() - std::time::Duration::from_secs(1))
        .is_err());
    fixture.retained();
    // Explicit test teardown through the same owner, not a foreign receipt.
    owner.stop(deadline()).unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    send.send(owner)
        .unwrap_or_else(|_| panic!("receiver exists"));
    drop(receive); // Lose the asynchronous owned result; Drop must retain resources.
    fixture.retained();
    assert!(Arc::strong_count(&fixture.custody) > 1);

    let legacy = Fixture::new(2);
    legacy.custody.process_may_exist();
    assert!(matches!(
        legacy.spawn_for(legacy.custody.actor, bwrap()),
        Err(ScopedSpawnError::AlreadyClaimed)
    ));
    legacy.custody.actor_stopped(&cancelled());
    let table = legacy.custody.bindings.clone();
    let id = legacy.tree.id().clone();
    drop(legacy.custody);
    assert!(table.lock().current(&id).is_some());
}

#[test]
fn scoped_custody_pin_error_retains_owner() {
    let fixture = Fixture::new(1);
    // Deterministic protocol-failure injection: a real exited wrapper emits no
    // init record. Successful pin/timeout tests above use actual bwrap.
    let mut owner = fixture
        .spawn_for(fixture.custody.actor, "/run/current-system/sw/bin/false")
        .unwrap();
    assert!(owner.pin(deadline()).is_err());
    assert!(owner.stop(deadline()).is_err());
    fixture.retained();
    drop(owner);
    fixture.retained();
    assert!(Arc::strong_count(&fixture.custody) > 1);
}
