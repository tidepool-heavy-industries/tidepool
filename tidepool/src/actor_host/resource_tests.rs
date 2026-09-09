use super::*;
use tidepool_node::command_resources::{CommandResourcePolicy, CommandResources};

#[tokio::test]
#[ignore = "requires delegated cgroups and the repository extractor setup"]
async fn resource_admission_timeout_and_cancellation_create_no_native_launch() {
    let resources = CommandResources::delegated(CommandResourcePolicy {
        machine_headroom_bytes: 1 << 60,
        queue_timeout_seconds: 1,
        ..Default::default()
    })
    .unwrap();
    let mut selected = None;
    let campaign = test_campaign::TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.command_resources = Some(resources);
            selected = Some(config.clone());
        },
    )
    .await;
    let config = selected.unwrap();
    let actor = campaign.actor.identity();
    let actor_root = config
        .run_root
        .join(format!("{}-{}", actor.id.0, actor.incarnation.0));
    let context = InteractiveLaunchContext {
        base_prompt: FrozenBasePrompt::materialize(&config.run_root).unwrap(),
        root: actor,
        run_root: config.run_root.clone(),
        tmux: TmuxSession::new("resource-test-must-not-launch").unwrap(),
        backend: native_interactive_backend(config.interactive_agent.clone()),
        worktrees: campaign.worktrees.clone(),
        bindings: campaign.bindings.clone(),
        config,
    };
    for cancel_after in [None, Some(Duration::from_millis(50))] {
        let hosted = Arc::new(Mutex::new(None));
        let pane = Arc::new(Mutex::new(None));
        let process = Arc::new(Mutex::new(scoped_custody::ScopedProcessSlot::Reserved));
        let (cancel, cancelled) = oneshot::channel();
        let launch = launch_interactive_application(
            campaign.root_installation.clone(),
            context.clone(),
            cancelled,
            InteractiveInheritance {
                thread: None,
                build_snapshot: None,
            },
            InteractiveLaunchRetention {
                hosted: hosted.clone(),
                pane: pane.clone(),
                process: process.clone(),
            },
        );
        tokio::pin!(launch);
        if let Some(delay) = cancel_after {
            tokio::select! {
                _ = &mut launch => panic!("launch completed before cancellation"),
                _ = tokio::time::sleep(delay) => {}
            }
            cancel.send(NativeRetirement::Terminate).unwrap();
            assert!(launch.await.unwrap().is_none());
        } else {
            let result = launch.await;
            assert!(
                matches!(result, Err(error) if error.to_string().contains("actor not started"))
            );
            drop(cancel);
        }
        assert!(!actor_root.exists());
        assert!(hosted.lock().is_none());
        assert!(pane.lock().is_none());
        assert!(matches!(
            *process.lock(),
            scoped_custody::ScopedProcessSlot::Reserved
        ));
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[test]
fn source_checkout_launch_has_process_custody_without_a_worktree_lease() {
    let actor = ActorRef {
        id: tidepool_actor::ActorId(0),
        incarnation: tidepool_actor::Incarnation(1),
    };
    let mut owner = InteractiveApplicationOwner {
        creator_workspace: None,
        cancel: None,
        native_retirement: Default::default(),
        pane: Arc::new(Mutex::new(None)),
        fork_gate: None,
        custody: None,
        scoped_retention: None,
        hosted: Arc::new(Mutex::new(None)),
        launch: HostLaunchState::Pending,
        terminal: None,
        retirement: Arc::new(Mutex::new(None)),
    };
    assert!(matches!(
        owner.reserve_scope(ActorWorkspaceRequest::Worktree("missing"), actor),
        Err(scoped_custody::ScopedClaimError::MissingLease)
    ));
    let slot = owner
        .reserve_scope(ActorWorkspaceRequest::SourceCheckout, actor)
        .unwrap();
    assert!(matches!(
        *slot.lock(),
        scoped_custody::ScopedProcessSlot::Reserved
    ));
    assert!(matches!(
        owner.reserve_scope(ActorWorkspaceRequest::SourceCheckout, actor),
        Err(scoped_custody::ScopedClaimError::AlreadyClaimed)
    ));
}
