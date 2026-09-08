use super::*;
use tidepool_actor::{ForkWorkspaceCustody, ResidentToolEndpoint};

pub(super) fn custody_fixture() -> (
    tidepool_worktree::testing::TestRepo,
    tempfile::TempDir,
    WorktreeHandle,
    Arc<Mutex<BindingTable>>,
    Arc<ActorForkWorkspaceAdmission>,
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
    let admission = fork_workspace_admission(
        manager,
        authority,
        bindings.clone(),
        "custody-test".into(),
        None,
    );
    (repository, runtime, tree, bindings, admission)
}

#[tokio::test(flavor = "current_thread")]
async fn custody_admission_waits_for_git_without_blocking_the_runtime() {
    let (_repo, _runtime, tree, _bindings, admission) = custody_fixture();
    let owner = ActorRef::first(tidepool_actor::ActorId(7));
    let _custody = admission
        .install_custody(owner, tree.id().as_str())
        .unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let worktrees = admission.worktrees.clone();
    let holder = std::thread::spawn(move || {
        let _guard = worktrees.lock();
        entered.send(()).unwrap();
        released.blocking_recv().unwrap();
    });
    ready.await.unwrap();
    let mut preparation = admission.admit(
        owner,
        "root/async-child".into(),
        ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
    );
    std::future::poll_fn(|cx| {
        assert!(preparation.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    release.send(()).unwrap();
    let prepared = tokio::time::timeout(Duration::from_secs(10), preparation)
        .await
        .unwrap()
        .unwrap();
    holder.join().unwrap();
    let handle = admission
        .manager
        .lookup(&WorktreeId::from_raw(
            &prepared.handle().handle_receipt.tree_id.raw,
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(handle.cwd().join("README.md")).unwrap(),
        "seed"
    );
    assert_ne!(handle.id(), tree.id());
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

#[derive(Clone)]
struct DelayedCustody {
    phase: InstallPhase,
    inner: Arc<dyn ForkWorkspaceAdmission>,
    entered: mpsc::UnboundedSender<(ActorRef, oneshot::Sender<()>)>,
}

impl ForkWorkspaceAdmission for DelayedCustody {
    fn admit(
        &self,
        owner: ActorRef,
        path: String,
        seed: ForkWorkspaceSeed,
    ) -> tidepool_actor::ForkWorkspaceAdmissionFuture<'_> {
        let controller = self.clone();
        Box::pin(async move {
            let prepared = self.inner.admit(owner, path, seed).await?;
            Ok(tidepool_actor::PreparedForkWorkspace::new(
                prepared.handle().clone(),
                move |actor| controller.delay_installation(actor, || prepared.install(actor)),
            ))
        })
    }
    fn install_custody(
        &self,
        _actor: ActorRef,
        _worktree: &str,
    ) -> Result<Arc<dyn ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        Err(ForkWorkspaceAdmissionError {
            detail: "admitted child must consume its owned preparation".into(),
        })
    }
}

impl DelayedCustody {
    fn delay_installation(
        &self,
        actor: ActorRef,
        install: impl FnOnce() -> Result<Arc<dyn ForkWorkspaceCustody>, ForkWorkspaceAdmissionError>,
    ) -> Result<Arc<dyn ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        let mut install = Some(install);
        let installed = match self.phase {
            InstallPhase::BeforeBind => None,
            InstallPhase::AfterBind => Some(install.take().unwrap()()?),
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
            None => install.take().unwrap()(),
        }
    }
}

// Keep activation observations rather than consuming them while waiting for attachment:
// attachment is earlier than RunRequest's initial worktreeHead.
async fn custody_activation(
    deployments: &mut mpsc::UnboundedReceiver<LocalResidentDeployment>,
) -> tidepool_actor::ResidentActivation {
    tokio::time::timeout(Duration::from_secs(120), async {
        match deployments.recv().await.expect("deployment stream") {
            LocalResidentDeployment::SessionReady { activation } => activation,
            LocalResidentDeployment::Retired { actor, terminal } => {
                panic!("{actor:?} retired before activation: {terminal:?}")
            }
            _ => panic!("unexpected event before activation"),
        }
    })
    .await
    .expect("exact request activation")
}

// Exhaustive diagnostics keep new deployment variants visible to this regression.
fn custody_event_description(event: &LocalResidentDeployment) -> String {
    match event {
        LocalResidentDeployment::PolicyInstalled(child) => {
            format!("PolicyInstalled {:?}", child.actor.identity())
        }
        LocalResidentDeployment::SessionReady { activation } => {
            format!("SessionReady {activation:?}")
        }
        LocalResidentDeployment::NotificationSend(_) => "NotificationSend".into(),
        LocalResidentDeployment::NotificationPoll(_) => "NotificationPoll".into(),
        LocalResidentDeployment::RequestUpdate { .. } => "RequestUpdate".into(),
        LocalResidentDeployment::ChildExited { notice } => format!(
            "ChildExited owner={:?} child={:?} terminal={:?}",
            notice.owner,
            notice.child.identity(),
            notice.terminal
        ),
        LocalResidentDeployment::WatchChanged { notification } => {
            format!("WatchChanged {notification:?}")
        }
        LocalResidentDeployment::RequestCancellation { notification } => {
            format!("RequestCancellation {notification:?}")
        }
        LocalResidentDeployment::Retired { actor, terminal } => {
            format!("Retired {actor:?} {terminal:?}")
        }
    }
}

async fn custody_assert_request(
    policy: &dyn ResidentToolEndpoint,
    expression: &str,
    activation: &tidepool_actor::ResidentActivation,
) {
    let result = tests::dispatch_haskell_script(
        policy,
        &format!("inspectFull (requestId (forkedResponse {expression}))"),
    )
    .await;
    assert_eq!(result["status"], "committed", "{result:?}");
    assert_eq!(
        result["items"][0]["output"].as_str().unwrap().trim(),
        format!("RequestId {}", activation.request.0),
        "{result:?}"
    );
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
        assert!(
            campaign.deployments.try_recv().is_err(),
            "publication before custody"
        );
        release.send(()).unwrap();
        let child = tokio::time::timeout(Duration::from_secs(120), campaign.deployments.recv())
            .await
            .unwrap()
            .unwrap();
        let LocalResidentDeployment::PolicyInstalled(child) = child else {
            panic!("expected attachment");
        };
        assert_eq!(child.actor.identity(), actor);
        assert!(child.worktree_custody.is_some());
        campaign
            .authority
            .install_grant(actor.into(), worktree_grant(child.effective_role.role()));
        installed.push(child);
    }
    // Neither child can run before the whole fork boundary is acknowledged.
    assert!(campaign.deployments.try_recv().is_err());
    for child in &installed {
        child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    }
    let result = tokio::time::timeout(Duration::from_secs(120), launched)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    installed.sort_by(|a, b| a.label.cmp(&b.label));
    assert!(installed[0].label.ends_with("/first"));
    assert!(installed[1].label.ends_with("/second"));
    assert_ne!(installed[0].launch_worktrees, installed[1].launch_worktrees);
    let mut seen = [false; 2];
    for _ in 0..2 {
        let activation = custody_activation(&mut campaign.deployments).await;
        let index = installed
            .iter()
            .position(|child| child.actor.identity() == activation.id.actor())
            .expect("exact sibling actor");
        assert!(!seen[index], "duplicate sibling activation");
        seen[index] = true;
        custody_assert_request(
            campaign.root_installation.policy.as_ref(),
            if index == 0 {
                "(fst siblings)"
            } else {
                "(snd siblings)"
            },
            &activation,
        )
        .await;
    }
    assert_eq!(seen, [true, true]);

    // Give boundHead a distinguishable source, not the root/sibling seed.
    let parent_tree = campaign
        .worktrees
        .lookup(&WorktreeId::from_raw(&installed[0].launch_worktrees[0]))
        .unwrap()
        .unwrap();
    campaign
        .worktrees
        .git()
        .try_run(
            parent_tree.cwd(),
            &[
                "-c",
                "user.name=Custody Test",
                "-c",
                "user.email=custody@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "distinct nested source",
            ],
        )
        .unwrap();
    let sibling_tree = campaign
        .worktrees
        .lookup(&WorktreeId::from_raw(&installed[1].launch_worktrees[0]))
        .unwrap()
        .unwrap();
    let sibling_head = campaign
        .worktrees
        .git()
        .try_run(sibling_tree.cwd(), &["rev-parse", "HEAD"])
        .unwrap();

    let policy = installed[0].policy.clone();
    let nested = tokio::spawn(async move {
        tests::dispatch_haskell_script(policy.as_ref(), include_str!("custody_nested.hs")).await
    });
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
    assert!(
        campaign.deployments.try_recv().is_err(),
        "nested publication before custody"
    );
    release.send(()).unwrap();
    let child = tokio::time::timeout(Duration::from_secs(120), campaign.deployments.recv())
        .await
        .unwrap()
        .unwrap();
    let LocalResidentDeployment::PolicyInstalled(leaf) = child else {
        panic!("expected nested attachment");
    };
    assert_eq!(leaf.actor.identity(), actor);
    assert_eq!(leaf.context_parent, Some(installed[0].actor.identity()));
    campaign
        .authority
        .install_grant(actor.into(), worktree_grant(leaf.effective_role.role()));
    let leaf_tree = campaign
        .worktrees
        .lookup(&WorktreeId::from_raw(&leaf.launch_worktrees[0]))
        .unwrap()
        .unwrap();
    let parent_head = campaign
        .worktrees
        .git()
        .try_run(parent_tree.cwd(), &["rev-parse", "HEAD"])
        .unwrap();
    assert_eq!(leaf_tree.source_head().as_str(), parent_head.trimmed());
    assert_ne!(parent_head.trimmed(), sibling_head.trimmed());
    leaf.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(120), nested)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    let activation = custody_activation(&mut campaign.deployments).await;
    assert_eq!(activation.id.actor(), leaf.actor.identity());
    custody_assert_request(installed[0].policy.as_ref(), "nested", &activation).await;
    let watch = tests::dispatch_haskell_script(installed[0].policy.as_ref(),
        "let Right readyLabel = watchLabel \"custody-leaf-ready\"\nleafReady <- watch readyLabel (awaitFork nested)").await;
    assert_eq!(watch["status"], "committed", "{watch:?}");
    let reply =
        tests::dispatch_haskell_script(leaf.policy.as_ref(), "respond (sessionInput :: Text)")
            .await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    let event = tokio::time::timeout(Duration::from_secs(120), campaign.deployments.recv())
        .await
        .expect("leaf reply watch timeout")
        .expect("deployment stream");
    match event {
        LocalResidentDeployment::WatchChanged { notification } => {
            assert_eq!(notification.owner, installed[0].actor.identity());
            assert_eq!(notification.label, "custody-leaf-ready");
            assert_eq!(
                notification.transition,
                tidepool_actor::WatchTransition::Ready
            );
        }
        event => panic!(
            "unexpected event before shutdown: {}",
            custody_event_description(&event)
        ),
    }
    let reply = tests::dispatch_haskell_script(
        installed[0].policy.as_ref(),
        include_str!("custody_nested_reply.hs"),
    )
    .await;
    assert_eq!(reply["status"], "committed", "{reply:?}");
    assert_eq!(
        reply["items"].as_array().unwrap().last().unwrap()["output"]
            .as_str()
            .unwrap()
            .trim(),
        "True"
    );
    installed.push(leaf);
    let ids: Vec<_> = installed
        .iter()
        .map(|child| WorktreeId::from_raw(&child.launch_worktrees[0]))
        .collect();
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    // No lifecycle event is expected while these three applications are live.
    if let Ok(event) = campaign.deployments.try_recv() {
        panic!(
            "unexpected event before shutdown: {}",
            custody_event_description(&event)
        );
    }
    let root = campaign.actor.identity();
    let expected: std::collections::HashMap<_, _> = std::iter::once((
        root,
        tidepool_actor::ActorTerminal {
            kind: tidepool_actor::ActorExitKind::Cancelled,
            summary: "forest host shutdown".into(),
        },
    ))
    .chain(installed.iter().map(|child| {
        (
            child.actor.identity(),
            tidepool_actor::ActorTerminal {
                kind: tidepool_actor::ActorExitKind::Cancelled,
                summary: "owner actor stopped".into(),
            },
        )
    }))
    .collect();
    let owners: std::collections::HashMap<_, _> = installed
        .iter()
        .map(|child| {
            (
                child.actor.identity(),
                child.supervisor_parent.expect("child supervisor"),
            )
        })
        .collect();
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    // Shutdown joins linked cleanup before publishing the root terminal. Collect
    // by exact identity: neither retirement order nor ChildExited delivery order
    // is a contract (a shutting-down owner's mailbox may not consume the notice).
    let mut retired = std::collections::HashMap::new();
    let mut child_exits = std::collections::HashMap::new();
    while let Ok(event) = campaign.deployments.try_recv() {
        match event {
            LocalResidentDeployment::Retired { actor, terminal } => {
                assert_eq!(
                    expected.get(&actor),
                    Some(&terminal),
                    "unexpected retirement {actor:?}"
                );
                assert!(
                    retired.insert(actor, terminal).is_none(),
                    "duplicate retirement {actor:?}"
                );
            }
            LocalResidentDeployment::ChildExited { notice } => {
                let child = notice.child.identity();
                assert_eq!(
                    owners.get(&child),
                    Some(&notice.owner),
                    "unexpected child exit owner"
                );
                assert_eq!(
                    expected.get(&child),
                    Some(&notice.terminal),
                    "unexpected child exit terminal"
                );
                assert!(
                    child_exits.insert(child, notice.terminal).is_none(),
                    "duplicate child exit"
                );
            }
            event => panic!(
                "unexpected event during shutdown: {}",
                custody_event_description(&event)
            ),
        }
    }
    assert_eq!(
        retired, expected,
        "every exact root/sibling/leaf must retire"
    );
    assert_eq!(campaign.actor.terminal().get().as_ref(), retired.get(&root));
    for child in &installed {
        assert_eq!(
            child.actor.terminal().get().as_ref(),
            retired.get(&child.actor.identity())
        );
    }
    drop(installed);
    for id in ids {
        assert!(campaign.bindings.lock().current(&id).is_none());
        assert!(campaign
            .worktrees
            .lookup(&id)
            .unwrap()
            .unwrap()
            .cwd()
            .join("README.md")
            .exists());
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
async fn failed_coordination_preserves_native_pane_and_recovery_requires_observed_exit() {
    struct ServerCleanup(String);
    impl Drop for ServerCleanup {
        fn drop(&mut self) {
            let _ = std::process::Command::new("tmux")
                .args(["-L", &self.0, "kill-server"])
                .output();
        }
    }
    let socket = format!("shoal-containment-{}", uuid::Uuid::new_v4().simple());
    let _cleanup = ServerCleanup(socket.clone());
    let tmux = TmuxSession::with_socket("containment", &socket).unwrap();
    let root = tempfile::tempdir().unwrap();
    let boundary = ProcessMountBoundary::new(
        root.path(),
        [root.path().to_path_buf()],
        [root.path().to_path_buf()],
    )
    .unwrap();
    let command = boundary.wrap(
        BUBBLEWRAP_PROGRAM,
        ProcessInvocation {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "while [ ! -f exit ]; do sleep 0.05; done; printf survived > survived".into(),
            ],
        },
    );
    let pane = tmux
        .create(&TmuxLaunch {
            window_name: "native".into(),
            cwd: root.path().into(),
            program: command.program,
            args: command.args,
            environment: Default::default(),
            unset_environment: Default::default(),
        })
        .await
        .unwrap();
    tmux.retain_pane_on_exit(&pane).await.unwrap();
    assert!(confirm_native_exit(&tmux, Some(&pane)).await.is_err());
    let outcome = retire_native_pane(&tmux, &pane, NativeRetirement::Preserve).await;
    assert!(matches!(outcome, CleanupComponentOutcome::Failed { .. }));
    assert!(!tmux.pane_status(&pane).await.unwrap().unwrap().dead);
    assert!(confirm_native_exit(&tmux, None).await.is_err());
    std::fs::write(root.path().join("exit"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while confirm_native_exit(&tmux, Some(&pane)).await.is_err() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("survived")).unwrap(),
        "survived"
    );
    let _ = retire_native_pane(&tmux, &pane, NativeRetirement::Terminate).await;
    assert!(tmux.pane_status(&pane).await.unwrap().is_none());
    assert!(confirm_native_exit(&tmux, Some(&pane)).await.is_err());
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
        let _ = retire_native_pane(&owned, &pane, NativeRetirement::Terminate).await;
        drop(custody);
        assert!(bindings.lock().current(tree.id()).is_some());
        assert!(foreign.list_panes().await.unwrap().contains(&foreign_pane));
    }
}
