#![allow(
    clippy::disallowed_methods,
    reason = "test: launches real tmux/process fixtures directly, not through the production launcher"
)]
use super::*;
use exomonad_actor::{ForkWorkspaceCustody, ResidentToolEndpoint};
use exomonad_worktree::WorktreeHandle;

pub(super) fn custody_fixture() -> (
    exomonad_worktree::testing::TestRepo,
    tempfile::TempDir,
    WorktreeHandle,
    Arc<Mutex<BindingTable>>,
    Arc<ActorForkWorkspaceAdmission>,
) {
    let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("README.md", "seed", "seed")
        .unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, bindings) = actor_worktree_resources_at(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path()).unwrap(),
        repository.path(),
    )
    .unwrap();
    let tree = manager
        .create(&exomonad_worktree::WorktreeSpec::from_current_repository(
            "custody",
        ))
        .unwrap();
    let bindings = Arc::new(Mutex::new(bindings));
    let authority = ActorWorktreeAuthority::new("custody-test", bindings.clone());
    let admission =
        fork_workspace_admission(manager, authority, bindings.clone(), "custody-test".into());
    (repository, runtime, tree, bindings, admission)
}

/// The root actor writes its own runtime state (journal, logs) straight into
/// the source repository it was handed — it is never admitted through
/// `prepare()` the way a fork's checkout is, so nothing on that path installs
/// the exclusion. A bootstrapped campaign must still leave the source clean:
/// `actor_worktree_resources_at` installs the exclusion once, for every
/// caller that builds a `WorktreeManager` over a source repository, rather
/// than depending on each caller (a launcher, a scaffold, a test harness) to
/// remember it separately.
#[tokio::test(flavor = "multi_thread")]
async fn bootstrapping_a_campaign_leaves_the_source_repository_clean() {
    let campaign = test_campaign::TestCampaign::start().await;
    let git = exomonad_worktree::GitCli::new();
    let dirty =
        exomonad_worktree::git::inspect::dirty_summary(&git, campaign._repository.path()).unwrap();
    assert!(
        dirty.is_clean(),
        "source repository dirty after bootstrap: {dirty}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn custody_admission_waits_for_git_without_blocking_the_runtime() {
    let (_repo, _runtime, tree, _bindings, admission) = custody_fixture();
    let owner = ActorRef::first(exomonad_actor::ActorId(7));
    let _custody = admission
        .install_custody(
            owner,
            tree.id().as_str(),
            exomonad_actor::WorkspaceAccess::ReadWrite,
        )
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
        ForkWorkspaceSeed::CurrentCheckout(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
        exomonad_actor::WorkspaceAccess::ReadWrite,
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
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let custody = admission
        .install_custody(
            actor,
            tree.id().as_str(),
            exomonad_actor::WorkspaceAccess::ReadWrite,
        )
        .unwrap();
    assert!(
        admission
            .install_custody(
                actor,
                tree.id().as_str(),
                exomonad_actor::WorkspaceAccess::ReadWrite
            )
            .is_err()
    );
    for other in [
        ActorRef::first(exomonad_actor::ActorId(8)),
        ActorRef {
            incarnation: exomonad_actor::Incarnation(2),
            ..actor
        },
    ] {
        assert!(
            admission
                .install_custody(
                    other,
                    tree.id().as_str(),
                    exomonad_actor::WorkspaceAccess::ReadWrite
                )
                .is_err()
        );
    }
    let host = custody.clone();
    drop(host);
    assert!(bindings.lock().current(tree.id()).is_some());
    drop(custody);
    assert!(bindings.lock().current(tree.id()).is_none());
    assert!(tree.cwd().join("README.md").exists());
    let rebound = admission
        .install_custody(
            actor,
            tree.id().as_str(),
            exomonad_actor::WorkspaceAccess::ReadWrite,
        )
        .unwrap();
    drop(rebound);
    assert!(bindings.lock().current(tree.id()).is_none());
}

#[test]
fn custody_retains_binding_when_process_cleanup_is_uncertain() {
    let (_repo, _runtime, tree, bindings, admission) = custody_fixture();
    let custody = admission
        .install_custody(
            ActorRef::first(exomonad_actor::ActorId(7)),
            tree.id().as_str(),
            exomonad_actor::WorkspaceAccess::ReadWrite,
        )
        .unwrap();
    custody.process_may_exist();
    drop(custody);
    assert!(bindings.lock().current(tree.id()).is_some());
}

#[test]
fn custody_rejects_missing_worktrees_without_binding() {
    let (_repo, _runtime, _tree, bindings, admission) = custody_fixture();
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    for tree in ["../outside", "wt-absent"] {
        assert!(
            admission
                .install_custody(actor, tree, exomonad_actor::WorkspaceAccess::ReadWrite)
                .is_err()
        );
        assert!(
            bindings
                .lock()
                .current(&WorktreeId::from_raw(tree))
                .is_none()
        );
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
        policy: exomonad_actor::WorkspaceAccess,
    ) -> exomonad_actor::ForkWorkspaceAdmissionFuture<'_> {
        let controller = self.clone();
        Box::pin(async move {
            let prepared = self.inner.admit(owner, path, seed, policy).await?;
            Ok(exomonad_actor::PreparedForkWorkspace::new(
                prepared.handle().clone(),
                move |actor| controller.delay_installation(actor, || prepared.install(actor)),
            ))
        })
    }
    fn install_custody(
        &self,
        _actor: ActorRef,
        _worktree: &str,
        _access: exomonad_actor::WorkspaceAccess,
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
    campaign: &mut test_campaign::TestCampaign,
) -> exomonad_actor::ResidentActivation {
    campaign
        .next_deployment(
            "exact request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation } => Ok(activation),
                LocalResidentDeployment::Retired { actor, terminal } => {
                    panic!("{actor:?} retired before activation: {terminal:?}")
                }
                other => Err(other),
            },
        )
        .await
}

// Exhaustive diagnostics keep new deployment variants visible to this regression.
fn custody_event_description(event: &LocalResidentDeployment) -> String {
    match event {
        LocalResidentDeployment::DisplayPublished(_) => "DisplayPublished".into(),
        LocalResidentDeployment::PolicyInstalled(child) => {
            format!("PolicyInstalled {:?}", child.actor.identity())
        }
        LocalResidentDeployment::SessionReady { activation } => {
            format!("SessionReady {activation:?}")
        }
        LocalResidentDeployment::NotificationSend(_) => "NotificationSend".into(),
        LocalResidentDeployment::CommandBackend(_) => "CommandBackend".into(),
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
        LocalResidentDeployment::SettlementChanged { notification } => {
            format!("SettlementChanged {notification:?}")
        }
        LocalResidentDeployment::RequestCancellation { notification } => {
            format!("RequestCancellation {notification:?}")
        }
        LocalResidentDeployment::Retired { actor, terminal } => {
            format!("Retired {actor:?} {terminal:?}")
        }
        LocalResidentDeployment::ReleaseAwait(request) => {
            format!("ReleaseAwait {:?}", request.actor)
        }
    }
}

async fn custody_assert_request(
    policy: &dyn ResidentToolEndpoint,
    expression: &str,
    activation: &exomonad_actor::ResidentActivation,
) {
    let result =
        tests::dispatch_haskell_script(policy, &format!("inspectFull (requestId {expression})"))
            .await;
    assert_eq!(result["status"], "committed", "{result:?}");
    assert_eq!(
        result["items"][0]["output"].as_str().unwrap().trim(),
        format!("RequestId {}", activation.request.0),
        "{result:?}"
    );
}

#[tokio::test]
async fn inherited_response_late_fill_and_release_preserve_extracted_value() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let first = tests::dispatch_haskell_script(
        root.as_ref(),
        include_str!("inherited_response_producer.hs"),
    )
    .await;
    assert_eq!(first["status"], "committed", "{first:?}");
    let producer = campaign
        .next_deployment(
            "producer installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        producer.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
            integrate: true,
        },
    );
    producer.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    custody_activation(&mut campaign).await;

    // The observer's inherited source tip includes `worker`, whose typed
    // ExitCell is still pending at this fork boundary.
    let second = tests::dispatch_haskell_script(
        root.as_ref(),
        include_str!("inherited_response_observer.hs"),
    )
    .await;
    assert_eq!(second["status"], "committed", "{second:?}");
    let observer = campaign
        .next_deployment(
            "observer installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        observer.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
            integrate: true,
        },
    );
    observer.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    custody_activation(&mut campaign).await;

    let watch = tests::dispatch_haskell_script(
        observer.policy.as_ref(),
        "inheritedWatch <- watch (\"inherited-ready\" :: WatchLabel) (awaitResponse worker)",
    )
    .await;
    assert_eq!(watch["status"], "committed", "{watch:?}");
    let pending =
        tests::dispatch_haskell_script(observer.policy.as_ref(), "pollWatch inheritedWatch").await;
    assert_eq!(pending["status"], "committed", "{pending:?}");
    assert!(pending.to_string().contains("WatchPending"), "{pending:?}");

    let reply = tests::dispatch_haskell_script(
        producer.policy.as_ref(),
        "respond ((sessionInput :: Text), (\\x -> x + (1 :: Int)))",
    )
    .await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    let observer_id = observer.actor.identity();
    campaign
        .next_deployment(
            "foreign watch readiness",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::WatchChanged { notification }
                    if notification.owner == observer_id
                        && notification.label == "inherited-ready" =>
                {
                    assert_eq!(
                        notification.transition,
                        exomonad_actor::WatchTransition::Ready
                    );
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let root_id = campaign.root_installation.actor.identity();
    campaign
        .next_deployment(
            "owner settlement notice",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SettlementChanged { notification }
                    if notification.owner == root_id =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let extracted = tests::dispatch_haskell_script(
        observer.policy.as_ref(),
        include_str!("inherited_response_read.hs"),
    )
    .await;
    assert_eq!(extracted["status"], "committed", "{extracted:?}");
    assert!(extracted.to_string().contains("custody"), "{extracted:?}");
    assert!(extracted.to_string().contains("42"), "{extracted:?}");

    let queued = tests::dispatch_haskell_script(
        observer.policy.as_ref(),
        "queuedWatch <- watch (\"queued-ready\" :: WatchLabel) (awaitResponse worker)",
    )
    .await;
    assert_eq!(queued["status"], "committed", "{queued:?}");

    let handoff = tests::dispatch_haskell_script(observer.policy.as_ref(), "respond worker").await;
    assert_eq!(handoff["status"], "replied", "{handoff:?}");
    let returned = tests::dispatch_haskell_script(
        root.as_ref(),
        "forwardedState <- pollResponse observer\nlet forwarded = case forwardedState of { ResponseReady answer -> responseValue answer; _ -> error \"typed handoff was not ready\" }\nforwardedState2 <- pollResponse forwarded\ninspectFull (case forwardedState2 of { ResponseReady answer -> let (label, run) = responseValue answer in (label, run 41); _ -> error \"forwarded response was not ready\" })",
    ).await;
    assert_eq!(returned["status"], "committed", "{returned:?}");
    assert!(returned.to_string().contains("42"), "{returned:?}");

    let released = tests::dispatch_haskell_script(root.as_ref(), "forgetResponse worker").await;
    assert_eq!(released["status"], "committed", "{released:?}");
    assert!(
        released.to_string().contains("ResponseForgotten"),
        "{released:?}"
    );
    let expired = tests::dispatch_haskell_script(
        observer.policy.as_ref(),
        "expired <- pollResponse worker\ninspectFull expired\ninspectFull (fst retained, snd retained 41)",
    )
    .await;
    assert_eq!(expired["status"], "committed", "{expired:?}");
    assert!(expired.to_string().contains("ReplyStale"), "{expired:?}");
    assert!(expired.to_string().contains("custody"), "{expired:?}");
    assert!(expired.to_string().contains("42"), "{expired:?}");
    let forwarded_expired =
        tests::dispatch_haskell_script(root.as_ref(), "pollResponse forwarded").await;
    assert_eq!(
        forwarded_expired["status"], "committed",
        "{forwarded_expired:?}"
    );
    assert!(
        forwarded_expired.to_string().contains("ReplyStale"),
        "{forwarded_expired:?}"
    );
    let queued_after_release =
        tests::dispatch_haskell_script(observer.policy.as_ref(), "pollWatch queuedWatch").await;
    assert_eq!(
        queued_after_release["status"], "committed",
        "{queued_after_release:?}"
    );
    assert!(
        queued_after_release
            .to_string()
            .contains("ResponseReleased"),
        "{queued_after_release:?}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn custody_precedes_first_bootstrap_worktree_use_for_two_siblings() {
    let (entered, mut installing) = mpsc::unbounded_channel();
    let mut campaign = test_campaign::TestCampaign::start_with_admission(|inner| {
        Arc::new(DelayedCustody {
            inner,
            entered,
            phase: InstallPhase::BeforeBind,
        })
    })
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
        assert!(
            campaign
                .bindings
                .lock()
                .active_for_agent(&WorktreePrincipal::exact_actor(
                    &runtime_namespace(campaign.session_root.path()),
                    actor.id.0,
                    actor.incarnation.0,
                ))
                .is_none()
        );
        campaign.assert_no_deployment("publication before custody", |_| true);
        release.send(()).unwrap();
        let child = campaign
            .next_deployment(
                "sibling attachment",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                    LocalResidentDeployment::ChildExited { notice } => {
                        panic!("expected attachment, got ChildExited: {notice:?}")
                    }
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        panic!("expected attachment, got Retired {actor}: {terminal:?}")
                    }
                    other => Err(other),
                },
            )
            .await;
        assert_eq!(child.actor.identity(), actor);
        assert!(child.worktree_custody.is_some());
        campaign.authority.install_grant(
            actor.into(),
            ActorWorktreeGrant::Bound {
                enumerate: false,
                allocate: true,
                integrate: true,
            },
        );
        installed.push(child);
    }
    // Neither child can run before the whole fork boundary is acknowledged.
    // A child may already park and queue its activation (SessionReady only
    // appends to its durable inbox); its provider session is bound, and so
    // reads that inbox, only after the gate commits (the host's binding
    // discovery waits on `wait_committed`). What must not happen yet is any
    // child finishing or retiring.
    let children: Vec<_> = installed
        .iter()
        .map(|child| child.actor.identity())
        .collect();
    campaign.assert_no_deployment("fork boundary not yet acknowledged", |event| match event {
        LocalResidentDeployment::ChildExited { notice } => {
            children.contains(&notice.child.identity())
        }
        LocalResidentDeployment::Retired { actor, .. } => children.contains(actor),
        _ => false,
    });
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
        let activation = custody_activation(&mut campaign).await;
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

    // Give currentCheckout a distinguishable source, not the root/sibling seed.
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
    assert!(
        campaign
            .bindings
            .lock()
            .active_for_agent(&WorktreePrincipal::exact_actor(
                &runtime_namespace(campaign.session_root.path()),
                actor.id.0,
                actor.incarnation.0,
            ))
            .is_none()
    );
    campaign.assert_no_deployment("nested publication before custody", |_| true);
    release.send(()).unwrap();
    let leaf = campaign
        .next_deployment(
            "nested attachment",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(leaf) => Ok(leaf),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(leaf.actor.identity(), actor);
    assert_eq!(leaf.context_parent, Some(installed[0].actor.identity()));
    campaign.authority.install_grant(
        actor.into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
            integrate: true,
        },
    );
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
    let activation = custody_activation(&mut campaign).await;
    assert_eq!(activation.id.actor(), leaf.actor.identity());
    custody_assert_request(installed[0].policy.as_ref(), "nested", &activation).await;
    let watch = tests::dispatch_haskell_script(
        installed[0].policy.as_ref(),
        "let readyLabel = \"custody-leaf-ready\" :: WatchLabel\nleafReady <- watch readyLabel (awaitResponse nested)",
    )
    .await;
    assert_eq!(watch["status"], "committed", "{watch:?}");
    let reply =
        tests::dispatch_haskell_script(leaf.policy.as_ref(), "respond (sessionInput :: Text)")
            .await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    let owner = installed[0].actor.identity();
    campaign
        .next_deployment(
            "leaf reply watch",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::WatchChanged { notification }
                    if notification.owner == owner
                        && notification.label == "custody-leaf-ready" =>
                {
                    assert_eq!(
                        notification.transition,
                        exomonad_actor::WatchTransition::Ready
                    );
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
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
    if let Some(event) = campaign.drain_ready().into_iter().next() {
        panic!(
            "unexpected event before shutdown: {}",
            custody_event_description(&event)
        );
    }
    let root = campaign.actor.identity();
    let expected: std::collections::HashMap<_, _> = std::iter::once((
        root,
        exomonad_actor::ActorTerminal {
            kind: exomonad_actor::ActorExitKind::Cancelled,
            summary: "forest host shutdown".into(),
            diagnostic: None,
        },
    ))
    .chain(installed.iter().map(|child| {
        (
            child.actor.identity(),
            exomonad_actor::ActorTerminal {
                kind: exomonad_actor::ActorExitKind::Cancelled,
                summary: "owner actor stopped".into(),
                diagnostic: None,
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
    (&mut campaign.hosted).await.unwrap();
    // Shutdown joins linked cleanup before publishing the root terminal. Collect
    // by exact identity: neither retirement order nor ChildExited delivery order
    // is a contract (a shutting-down owner's mailbox may not consume the notice).
    let mut retired = std::collections::HashMap::new();
    let mut child_exits = std::collections::HashMap::new();
    let mut settlement_notices = std::collections::HashSet::new();
    for event in campaign.drain_ready() {
        match event {
            LocalResidentDeployment::Retired { actor, terminal } => {
                // Owner stop and forest shutdown race to cancel each actor;
                // the terminal kind is the contract, not its summary text.
                assert_eq!(
                    expected.get(&actor).map(|expected| &expected.kind),
                    Some(&terminal.kind),
                    "unexpected retirement {actor:?}: {terminal:?}"
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
                    expected.get(&child).map(|expected| &expected.kind),
                    Some(&notice.terminal.kind),
                    "unexpected child exit terminal: {:?}",
                    notice.terminal
                );
                assert!(
                    child_exits.insert(child, notice.terminal).is_none(),
                    "duplicate child exit"
                );
            }
            // Stopping the children before the root cancels the root's
            // still-observing requests; a notify-owner request then publishes
            // a settlement notice to its owner. Delivery during
            // shutdown is best-effort, so the count is not asserted.
            LocalResidentDeployment::SettlementChanged { notification } => {
                assert_eq!(notification.owner, root, "settlement notice owner");
                assert!(
                    matches!(notification.label.as_str(), "first" | "second"),
                    "unexpected settlement notice label {}",
                    notification.label
                );
                // Which teardown path cancels first (owner stop or forest
                // shutdown) is a race; the invariant is the cancelled target.
                assert!(
                    matches!(
                        notification.transition,
                        exomonad_actor::SettlementTransition::Unavailable(
                            exomonad_actor::ResponseFailure::TargetCancelled(_)
                        )
                    ),
                    "settlement notice transition: {:?}",
                    notification.transition
                );
                assert!(
                    settlement_notices.insert(notification.request),
                    "duplicate settlement notice"
                );
            }
            event => panic!(
                "unexpected event during shutdown: {}",
                custody_event_description(&event)
            ),
        }
    }
    let mut retired_ids: Vec<_> = retired.keys().copied().collect();
    let mut expected_ids: Vec<_> = expected.keys().copied().collect();
    retired_ids.sort_by_key(|actor| (actor.id.0, actor.incarnation.0));
    expected_ids.sort_by_key(|actor| (actor.id.0, actor.incarnation.0));
    assert_eq!(
        retired_ids, expected_ids,
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
        assert!(
            campaign
                .worktrees
                .lookup(&id)
                .unwrap()
                .unwrap()
                .cwd()
                .join("README.md")
                .exists()
        );
    }
}

#[tokio::test]
async fn custody_install_failure_prevents_provider_publication() {
    let (entered, mut installing) = mpsc::unbounded_channel();
    let mut campaign = test_campaign::TestCampaign::start_with_admission(|inner| {
        Arc::new(DelayedCustody {
            inner,
            entered,
            phase: InstallPhase::BeforeBind,
        })
    })
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
    campaign
        .next_deployment(
            "custody-failure retirement",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => panic!(
                    "provider published despite custody failure: {:?}",
                    child.actor.identity()
                ),
                LocalResidentDeployment::Retired { actor: retired, .. } if retired == actor => {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    assert!(
        campaign
            .bindings
            .lock()
            .active_for_agent(&WorktreePrincipal::exact_actor(
                &runtime_namespace(campaign.session_root.path()),
                actor.id.0,
                actor.incarnation.0,
            ))
            .is_none()
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

async fn cancel_at_install_phase(phase: InstallPhase) {
    let (entered, mut installing) = mpsc::unbounded_channel();
    let mut campaign = test_campaign::TestCampaign::start_with_admission(|inner| {
        Arc::new(DelayedCustody {
            inner,
            entered,
            phase,
        })
    })
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
        tests::dispatch_haskell_script(policy.as_ref(), "stopAgent (responseActor worker)").await
    });
    // On this current-thread executor the stop handler runs from publishing
    // this posture through recording shutdown intent before its first await.
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if matches!(campaign.root_installation.runtime_observation.snapshot().workbench_posture,
                exomonad_actor::ActorWorkbenchPosture::AwaitingEffect { effect, .. } if effect == "stopAgent") {
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
    (&mut campaign.hosted).await.unwrap();
    for event in campaign.drain_ready() {
        if let LocalResidentDeployment::PolicyInstalled(child) = event {
            panic!(
                "provider published for cancelled bootstrap: {:?}",
                child.actor.identity()
            );
        }
    }
    assert!(
        campaign
            .bindings
            .lock()
            .active_for_agent(&principal)
            .is_none()
    );
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
        &crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
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
    assert!(
        campaign
            .bindings
            .lock()
            .active_for_agent(&installing_principal)
            .is_some()
    );
    release.send(()).unwrap();
    let result = launched.await.unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    let failed = campaign
        .next_deployment(
            "bootstrap-failure retirement",
            Duration::from_secs(120),
            |event| match event {
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
                    Ok(actor)
                }
                other => Err(other),
            },
        )
        .await;
    assert_eq!(failed, installing_actor);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    let principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(campaign.session_root.path()),
        failed.id.0,
        failed.incarnation.0,
    );
    assert!(
        campaign
            .bindings
            .lock()
            .active_for_agent(&principal)
            .is_none()
    );
}

#[tokio::test]
async fn a_resident_actor_may_write_its_own_worktree_and_nothing_else() {
    let (_repo, _runtime, tree, bindings, admission) = custody_fixture();
    let holder = ActorRef::first(exomonad_actor::ActorId(11));
    let _custody = admission
        .install_custody(
            holder,
            tree.id().as_str(),
            exomonad_actor::WorkspaceAccess::ReadWrite,
        )
        .unwrap();
    let authority = ActorWorktreeAuthority::new("custody-test", bindings);
    let source = admission.manager.source_repository().to_owned();

    let held = resident_command_roots(&authority, &admission.manager, &source, holder).unwrap();
    assert_eq!(held.directory, tree.cwd());
    assert!(held.custody);
    assert_eq!(
        held.writable,
        vec![tree.cwd().to_owned(), source.join(".git")],
        "its own worktree, and the git directory a publication moves a ref in"
    );

    // An actor with neither repository authority nor custody gets no write access.
    let without_custody = ActorRef::first(exomonad_actor::ActorId(12));
    let unheld =
        resident_command_roots(&authority, &admission.manager, &source, without_custody).unwrap();
    assert_eq!(unheld.directory, source);
    assert!(!unheld.custody);
    assert!(unheld.writable.is_empty());

    // The boundary carries the working directory, so it is built per command.
    // A directory the actor cannot reach is refused, not silently redirected.
    let boundary = exomonad_node::ProcessMountBoundary::new(
        tree.cwd(),
        held.protected.clone(),
        held.writable.clone(),
    )
    .expect("a held worktree bounds cleanly");
    let wrapped = boundary.wrap(
        exomonad_node::BUBBLEWRAP_PROGRAM,
        exomonad_node::ProcessInvocation {
            program: "git".into(),
            args: vec!["status".into()],
        },
    );
    let chdir = wrapped
        .args
        .windows(2)
        .find(|window| window[0] == "--chdir")
        .map(|window| window[1].clone());
    assert_eq!(
        chdir,
        Some(tree.cwd().to_string_lossy().into_owned()),
        "the wrapper chooses the directory, so it has to be this one: {wrapped:?}"
    );
    assert!(
        exomonad_node::ProcessMountBoundary::new(
            std::path::Path::new("/tmp"),
            held.protected,
            held.writable,
        )
        .is_err(),
        "a directory outside every root is refused"
    );
}

#[test]
fn resident_command_grants_allow_root_coding_and_preserve_checkout_isolation() {
    let (_repo, _runtime, child_tree, bindings, admission) = custody_fixture();
    let source = admission.manager.source_repository();
    let root_tree = admission
        .manager
        .root_allocations()
        .create(&exomonad_worktree::WorktreeSpec::from_current_repository(
            "root-coding",
        ))
        .unwrap();
    let other_tree = admission
        .manager
        .create(&exomonad_worktree::WorktreeSpec::from_current_repository(
            "other-child",
        ))
        .unwrap();
    let root = ActorRef::first(exomonad_actor::ActorId(21));
    let operator = ActorRef::first(exomonad_actor::ActorId(22));
    let ungranted = ActorRef::first(exomonad_actor::ActorId(23));
    let child = ActorRef::first(exomonad_actor::ActorId(24));
    let _custody = admission
        .install_custody(
            child,
            child_tree.id().as_str(),
            exomonad_actor::WorkspaceAccess::ReadWrite,
        )
        .unwrap();
    let authority = ActorWorktreeAuthority::new("custody-test", bindings);
    authority.install_grant(root.into(), ActorWorktreeGrant::Repository);
    authority.install_grant(operator.into(), ActorWorktreeGrant::RepositoryReadOnly);
    authority.install_grant(
        child.into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
            integrate: true,
        },
    );
    let bubblewrap = resolve_scope_bubblewrap(&BTreeMap::new()).unwrap();
    let write = |actor, cwd: &Path, file: &str| {
        let roots = resident_command_roots(&authority, &admission.manager, source, actor).unwrap();
        let boundary = ProcessMountBoundary::new(cwd, roots.protected, roots.writable).unwrap();
        let invocation = boundary.wrap(
            bubblewrap.to_string_lossy(),
            exomonad_node::ProcessInvocation {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    r#"printf actual-command-write > "$1""#.into(),
                    "smoke".into(),
                    file.into(),
                ],
            },
        );
        std::process::Command::new(invocation.program)
            .args(invocation.args)
            .output()
            .unwrap()
    };
    for cwd in [source, root_tree.cwd()] {
        let result = write(root, cwd, "root-write.txt");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("root-write.txt")).unwrap(),
            "actual-command-write"
        );
    }
    let own_write = write(child, child_tree.cwd(), "child-write.txt");
    assert!(
        own_write.status.success(),
        "{}",
        String::from_utf8_lossy(&own_write.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(child_tree.cwd().join("child-write.txt")).unwrap(),
        "actual-command-write"
    );
    for (actor, cwd) in [
        (root, child_tree.cwd()),
        (operator, source),
        (operator, root_tree.cwd()),
        (ungranted, source),
        (child, source),
        (child, other_tree.cwd()),
    ] {
        let result = write(actor, cwd, "denied-write.txt");
        assert!(
            !result.status.success(),
            "unexpected write authority for {actor} at {}",
            cwd.display()
        );
        assert!(!cwd.join("denied-write.txt").exists());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("Read-only file system"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    // Inspection authority attenuates even an existing exact checkout binding.
    authority.install_grant(child.into(), ActorWorktreeGrant::RepositoryReadOnly);
    let denied = write(child, child_tree.cwd(), "inspection-write.txt");
    assert!(!denied.status.success());
    assert!(!child_tree.cwd().join("inspection-write.txt").exists());
    // A successor incarnation does not inherit the predecessor's repository grant.
    let successor = ActorRef {
        incarnation: exomonad_actor::Incarnation(2),
        ..root
    };
    let denied = write(successor, source, "successor-write.txt");
    assert!(!denied.status.success());
    assert!(!source.join("successor-write.txt").exists());
}

#[test]
fn resident_commands_resolve_linked_repository_metadata_without_parent_write_grants() {
    let (_repo, runtime, linked_source, _bindings, _admission) = custody_fixture();
    let (manager, bindings) = actor_worktree_resources_at(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path())
            .unwrap()
            .child("linked-root")
            .unwrap(),
        linked_source.cwd(),
    )
    .unwrap();
    let authority = ActorWorktreeAuthority::new("linked-root", Arc::new(Mutex::new(bindings)));
    let root = ActorRef::first(exomonad_actor::ActorId(31));
    let operator = ActorRef::first(exomonad_actor::ActorId(32));
    authority.install_grant(root.into(), ActorWorktreeGrant::Repository);
    authority.install_grant(operator.into(), ActorWorktreeGrant::RepositoryReadOnly);
    let common =
        exomonad_worktree::git::inspect::git_common_dir(manager.git(), linked_source.cwd())
            .unwrap();
    assert!(linked_source.cwd().join(".git").is_file());
    assert!(!common.starts_with(linked_source.cwd()));
    let bubblewrap = resolve_scope_bubblewrap(&BTreeMap::new()).unwrap();
    for (actor, writable) in [(root, true), (operator, false)] {
        let roots =
            resident_command_roots(&authority, &manager, linked_source.cwd(), actor).unwrap();
        assert!(roots.protected.contains(&common));
        assert!(
            !roots
                .protected
                .contains(&common.parent().unwrap().to_owned())
        );
        assert_eq!(roots.writable.contains(&common), writable);
        let boundary =
            ProcessMountBoundary::new(linked_source.cwd(), roots.protected, roots.writable)
                .unwrap();
        let path = common.join(if writable {
            "root-metadata-smoke"
        } else {
            "operator-metadata-smoke"
        });
        let invocation = boundary.wrap(
            bubblewrap.to_string_lossy(),
            exomonad_node::ProcessInvocation {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    r#"printf metadata-write > "$1""#.into(),
                    "smoke".into(),
                    path.to_string_lossy().into_owned(),
                ],
            },
        );
        let result = std::process::Command::new(invocation.program)
            .args(invocation.args)
            .output()
            .unwrap();
        assert_eq!(
            result.status.success(),
            writable,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(path.exists(), writable);
    }
    assert!(
        resident_command_roots(
            &authority,
            &manager,
            &runtime.path().join("missing-repository"),
            root
        )
        .is_err()
    );
}
