use super::*;
use tidepool_actor::{CustodyRelease, ForkWorkspaceCustody};

fn custody_fixture() -> (
    tidepool_worktree::testing::TestRepo,
    tempfile::TempDir,
    WorktreeHandle,
    Arc<Mutex<BindingTable>>,
    Arc<dyn ForkWorkspaceAdmission>,
) {
    let repository = tidepool_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("README.md", "seed", "seed")
        .unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, bindings) =
        actor_worktree_resources_at(runtime.path(), repository.path()).unwrap();
    let tree = manager
        .create(&tidepool_worktree::WorktreeSpec::from_current_repository(
            "custody",
        ))
        .unwrap();
    let bindings = Arc::new(Mutex::new(bindings));
    let authority = ActorWorktreeAuthority::new("custody-test", bindings.clone());
    let admission =
        fork_workspace_admission(manager, authority, bindings.clone(), "custody-test".into());
    (repository, runtime, tree, bindings, admission)
}

#[test]
fn custody_is_exact_and_released_only_after_last_owner() {
    let (_repo, _runtime, tree, bindings, admission) = custody_fixture();
    let actor = ActorRef::first(tidepool_actor::ActorId(7));
    let custody = admission
        .install_custody(actor, tree.id().as_str())
        .unwrap();
    assert!(admission
        .install_custody(actor, tree.id().as_str())
        .is_err());
    for other in [
        ActorRef::first(tidepool_actor::ActorId(8)),
        ActorRef {
            incarnation: tidepool_actor::Incarnation(2),
            ..actor
        },
    ] {
        assert!(admission
            .install_custody(other, tree.id().as_str())
            .is_err());
    }
    let host = custody.clone();
    host.process_may_exist();
    assert_eq!(
        host.release_after_process().unwrap(),
        CustodyRelease::RetainedByActor
    );
    assert!(bindings.lock().current(tree.id()).is_some());
    drop(custody);
    assert!(bindings.lock().current(tree.id()).is_none());
    assert!(tree.cwd().join("README.md").exists());
    let rebound = admission
        .install_custody(actor, tree.id().as_str())
        .unwrap();
    assert_eq!(
        rebound.release_after_process().unwrap(),
        CustodyRelease::Released
    );
}

#[test]
fn custody_retains_binding_when_process_cleanup_is_uncertain() {
    let (_repo, _runtime, tree, bindings, admission) = custody_fixture();
    let custody = admission
        .install_custody(
            ActorRef::first(tidepool_actor::ActorId(7)),
            tree.id().as_str(),
        )
        .unwrap();
    custody.process_may_exist();
    drop(custody);
    assert!(bindings.lock().current(tree.id()).is_some());
}

#[test]
fn custody_rejects_missing_worktrees_without_binding() {
    let (_repo, _runtime, _tree, bindings, admission) = custody_fixture();
    let actor = ActorRef::first(tidepool_actor::ActorId(7));
    for tree in ["../outside", "wt-absent"] {
        assert!(admission.install_custody(actor, tree).is_err());
        assert!(bindings
            .lock()
            .current(&WorktreeId::from_raw(tree))
            .is_none());
    }
}

struct DelayedCustody {
    inner: Arc<dyn ForkWorkspaceAdmission>,
    entered: mpsc::UnboundedSender<(ActorRef, oneshot::Sender<()>)>,
}

impl ForkWorkspaceAdmission for DelayedCustody {
    fn admit(
        &self,
        owner: ActorRef,
        path: &str,
        seed: ForkWorkspaceSeed,
    ) -> Result<tidepool_bridge_effects::WtWorktreeHandle, ForkWorkspaceAdmissionError> {
        self.inner.admit(owner, path, seed)
    }
    fn install_custody(
        &self,
        actor: ActorRef,
        worktree: &str,
    ) -> Result<Arc<dyn ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        let (release, ready) = oneshot::channel();
        self.entered
            .send((actor, release))
            .map_err(|_| ForkWorkspaceAdmissionError {
                detail: "test custody controller dropped".into(),
            })?;
        ready
            .blocking_recv()
            .map_err(|_| ForkWorkspaceAdmissionError {
                detail: "test custody installation cancelled".into(),
            })?;
        self.inner.install_custody(actor, worktree)
    }
}

#[tokio::test]
async fn custody_precedes_first_bootstrap_worktree_use_for_two_siblings() {
    let (entered, mut installing) = mpsc::unbounded_channel();
    let mut campaign = test_campaign::TestCampaign::start_with_admission(
        tidepool_actor::ResearchPolicy::default(),
        |inner| Arc::new(DelayedCustody { inner, entered }),
    )
    .await;
    let policy = campaign.root_installation.policy.clone();
    let launched = tokio::spawn(async move {
        tests::dispatch_haskell_script(policy.as_ref(), include_str!("custody_siblings.hs")).await
    });
    let mut installed = Vec::new();
    for _ in 0..2 {
        let (actor, release) = tokio::time::timeout(Duration::from_secs(120), installing.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(campaign
            .bindings
            .lock()
            .active_for_agent(&WorktreePrincipal::exact_actor(
                &runtime_namespace(campaign.session_root.path()),
                actor.id.0,
                actor.incarnation.0,
            ))
            .is_none());
        while let Ok(event) = campaign.deployments.try_recv() {
            if let LocalResidentDeployment::PolicyInstalled(child) = event {
                panic!(
                    "provider published before custody release: {:?}",
                    child.actor.identity()
                );
            }
        }
        release.send(()).unwrap();
        let child = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                match campaign.deployments.recv().await.unwrap() {
                    LocalResidentDeployment::PolicyInstalled(child) => break child,
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        panic!("{actor:?}: {terminal:?}")
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(child.actor.identity(), actor);
        assert!(child.worktree_custody.is_some());
        child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
        installed.push(child);
    }
    let result = tokio::time::timeout(Duration::from_secs(120), launched)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    assert_ne!(installed[0].launch_worktrees, installed[1].launch_worktrees);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    let ids: Vec<_> = installed
        .iter()
        .map(|child| WorktreeId::from_raw(&child.launch_worktrees[0]))
        .collect();
    drop(installed);
    while campaign.deployments.try_recv().is_ok() {}
    for id in ids {
        assert!(campaign.bindings.lock().current(&id).is_none());
    }
}

#[tokio::test]
async fn custody_install_failure_prevents_provider_publication() {
    let (entered, mut installing) = mpsc::unbounded_channel();
    let mut campaign = test_campaign::TestCampaign::start_with_admission(
        tidepool_actor::ResearchPolicy::default(),
        |inner| Arc::new(DelayedCustody { inner, entered }),
    )
    .await;
    let policy = campaign.root_installation.policy.clone();
    let launched = tokio::spawn(async move {
        tests::dispatch_haskell_script(policy.as_ref(), include_str!("custody_siblings.hs")).await
    });
    let (actor, release) = tokio::time::timeout(Duration::from_secs(120), installing.recv())
        .await
        .unwrap()
        .unwrap();
    // Cancel the concrete installation, rather than sleeping until a presumed race.
    drop(release);
    let result = tokio::time::timeout(Duration::from_secs(120), launched)
        .await
        .unwrap()
        .unwrap();
    // The enclosing tool committed before deferred child bootstrap ran.
    assert_eq!(result["status"], "committed", "{result:?}");
    drop(installing);
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match campaign.deployments.recv().await.unwrap() {
                LocalResidentDeployment::PolicyInstalled(child) => panic!(
                    "provider published despite custody failure: {:?}",
                    child.actor.identity()
                ),
                LocalResidentDeployment::Retired { actor: retired, .. } if retired == actor => {
                    break
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(campaign
        .bindings
        .lock()
        .active_for_agent(&WorktreePrincipal::exact_actor(
            &runtime_namespace(campaign.session_root.path()),
            actor.id.0,
            actor.incarnation.0,
        ))
        .is_none());
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
