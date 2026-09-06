use super::*;
use tidepool_actor::ForkWorkspaceCustody;

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
    drop(host);
    assert!(bindings.lock().current(tree.id()).is_some());
    drop(custody);
    assert!(bindings.lock().current(tree.id()).is_none());
    assert!(tree.cwd().join("README.md").exists());
    let rebound = admission
        .install_custody(actor, tree.id().as_str())
        .unwrap();
    drop(rebound);
    assert!(bindings.lock().current(tree.id()).is_none());
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

#[derive(Clone, Copy)]
enum InstallPhase {
    BeforeBind,
    AfterBind,
}

struct DelayedCustody {
    phase: InstallPhase,
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
        let installed = match self.phase {
            InstallPhase::BeforeBind => None,
            InstallPhase::AfterBind => Some(self.inner.install_custody(actor, worktree)?),
        };
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
        match installed {
            Some(custody) => Ok(custody),
            None => self.inner.install_custody(actor, worktree),
        }
    }
}

#[tokio::test]
async fn custody_precedes_first_bootstrap_worktree_use_for_two_siblings() {
    let (entered, mut installing) = mpsc::unbounded_channel();
    let mut campaign = test_campaign::TestCampaign::start_with_admission(
        tidepool_actor::ResearchPolicy::default(),
        |inner| {
            Arc::new(DelayedCustody {
                inner,
                entered,
                phase: InstallPhase::BeforeBind,
            })
        },
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
        |inner| {
            Arc::new(DelayedCustody {
                inner,
                entered,
                phase: InstallPhase::BeforeBind,
            })
        },
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

async fn cancel_at_install_phase(phase: InstallPhase) {
    let (entered, mut installing) = mpsc::unbounded_channel();
    let mut campaign = test_campaign::TestCampaign::start_with_admission(
        tidepool_actor::ResearchPolicy::default(),
        |inner| {
            Arc::new(DelayedCustody {
                inner,
                entered,
                phase,
            })
        },
    )
    .await;
    let policy = campaign.root_installation.policy.clone();
    let launched = tokio::spawn(async move {
        tests::dispatch_haskell_script(policy.as_ref(), include_str!("custody_single.hs")).await
    });
    let (actor, release) = tokio::time::timeout(Duration::from_secs(120), installing.recv())
        .await
        .unwrap()
        .unwrap();
    let principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(campaign.session_root.path()),
        actor.id.0,
        actor.incarnation.0,
    );
    assert_eq!(
        campaign
            .bindings
            .lock()
            .active_for_agent(&principal)
            .is_some(),
        matches!(phase, InstallPhase::AfterBind)
    );
    assert_eq!(launched.await.unwrap()["status"], "committed");
    let policy = campaign.root_installation.policy.clone();
    let stopped = tokio::spawn(async move {
        tests::dispatch_haskell_script(policy.as_ref(), "stopAgent (forkedActor worker)").await
    });
    // On this current-thread executor the stop handler runs from publishing
    // this posture through recording shutdown intent before its first await.
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if matches!(campaign.root_installation.runtime_observation.snapshot().workbench_posture,
                tidepool_actor::ActorWorkbenchPosture::AwaitingEffect { effect, .. } if effect == "stopAgent") {
                break;
            }
            assert!(!stopped.is_finished(), "stop finished without entering the pending retirement");
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    release.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(120), stopped)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    assert!(
        result["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("StoppedNow"),
        "{result:?}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    while let Ok(event) = campaign.deployments.try_recv() {
        if let LocalResidentDeployment::PolicyInstalled(child) = event {
            panic!(
                "provider published for cancelled bootstrap: {:?}",
                child.actor.identity()
            );
        }
    }
    assert!(campaign
        .bindings
        .lock()
        .active_for_agent(&principal)
        .is_none());
}

#[tokio::test]
async fn custody_actor_cancellation_during_delayed_install_releases_exact_binding() {
    cancel_at_install_phase(InstallPhase::BeforeBind).await;
}

#[tokio::test]
async fn custody_actor_cancellation_after_binding_prevents_provider_publication() {
    cancel_at_install_phase(InstallPhase::AfterBind).await;
}

fn copy_fixture_sources(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture_sources(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn custody_haskell_bootstrap_failure_after_install_releases_binding() {
    let private_sources = tempfile::tempdir().unwrap();
    copy_fixture_sources(
        &crate::haskell_sources::ensure_shoal_haskell().unwrap(),
        private_sources.path(),
    );
    let agent_module = private_sources
        .path()
        .join("Tidepool/Actors/Internal/Agent.hs");
    let original = std::fs::read_to_string(&agent_module).unwrap();
    let initial = "\\() -> attachAgent Nothing";
    assert_eq!(original.matches(initial).count(), 1);
    std::fs::write(
        &agent_module,
        original.replace(initial, include_str!("custody_boot_failure.hs").trim()),
    )
    .unwrap();
    let (entered, mut installing) = mpsc::unbounded_channel();
    let mut campaign = test_campaign::TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |inner| {
            Arc::new(DelayedCustody {
                inner,
                entered,
                phase: InstallPhase::AfterBind,
            })
        },
        |config| config.haskell_root = private_sources.path().to_path_buf(),
    )
    .await;
    let root = campaign.root_installation.policy.clone();
    let launched = tokio::spawn(async move {
        tests::dispatch_haskell_script(root.as_ref(), include_str!("custody_single.hs")).await
    });
    let (installing_actor, release) =
        tokio::time::timeout(Duration::from_secs(120), installing.recv())
            .await
            .unwrap()
            .unwrap();
    let installing_principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(campaign.session_root.path()),
        installing_actor.id.0,
        installing_actor.incarnation.0,
    );
    assert!(campaign
        .bindings
        .lock()
        .active_for_agent(&installing_principal)
        .is_some());
    release.send(()).unwrap();
    let result = launched.await.unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    let failed = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match campaign.deployments.recv().await.unwrap() {
                LocalResidentDeployment::PolicyInstalled(child) => panic!(
                    "provider published after bootstrap error: {:?}",
                    child.actor.identity()
                ),
                LocalResidentDeployment::Retired { actor, terminal } => {
                    assert_eq!(terminal.kind, ActorExitKind::Failed);
                    assert!(
                        terminal
                            .summary
                            .contains("intentional Haskell bootstrap failure"),
                        "{terminal:?}"
                    );
                    break actor;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(failed, installing_actor);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    let principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(campaign.session_root.path()),
        failed.id.0,
        failed.incarnation.0,
    );
    assert!(campaign
        .bindings
        .lock()
        .active_for_agent(&principal)
        .is_none());
}

#[tokio::test]
async fn custody_missing_or_foreign_pane_never_clears_process_fence() {
    struct ServerCleanup(String);
    impl Drop for ServerCleanup {
        fn drop(&mut self) {
            let _ = std::process::Command::new("tmux")
                .args(["-L", &self.0, "kill-server"])
                .output();
        }
    }
    let socket = format!("custody-proof-{}", std::process::id());
    let _cleanup = ServerCleanup(socket.clone());
    let owned = TmuxSession::with_socket("custody-owned", &socket).unwrap();
    let foreign = TmuxSession::with_socket("custody-foreign", &socket).unwrap();
    let launch = TmuxLaunch {
        window_name: "worker".into(),
        cwd: std::env::temp_dir(),
        program: "sleep".into(),
        args: vec!["60".into()],
        environment: BTreeMap::new(),
        unset_environment: BTreeSet::new(),
    };
    owned.create(&launch).await.unwrap();
    let foreign_pane = foreign.create(&launch).await.unwrap();
    for pane in [
        TmuxPaneId::parse("%999999999").unwrap(),
        foreign_pane.clone(),
    ] {
        let (_repo, _runtime, tree, bindings, admission) = custody_fixture();
        let custody = admission
            .install_custody(
                ActorRef::first(tidepool_actor::ActorId(7)),
                tree.id().as_str(),
            )
            .unwrap();
        custody.process_may_exist();
        // Both return Ok from kill_pane without owning/reaping this process.
        owned.kill_pane(&pane).await.unwrap();
        let socket_root = tempfile::tempdir().unwrap();
        let service = tokio::spawn(async {
            std::future::pending::<Result<(), InteractiveApplicationError>>().await
        });
        abandon_interactive_application(&owned, &pane, service, socket_root.path()).await;
        drop(custody);
        assert!(bindings.lock().current(tree.id()).is_some());
        assert!(foreign.list_panes().await.unwrap().contains(&foreign_pane));
    }
}
