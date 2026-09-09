//! Real admission of an inspection-only research subtree under host configuration.
use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;

async fn research_child(
    campaign: &mut TestCampaign,
) -> (
    tidepool_actor::LocalResidentInstallation,
    Arc<dyn tidepool_actor::ForkWorkspaceCustody>,
) {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match campaign.deployments.recv().await.unwrap() {
                LocalResidentDeployment::PolicyInstalled(child) => {
                    let role = &child.effective_role;
                    assert_eq!(role.role(), tidepool_actor::ActorRole::Research);
                    assert_eq!(
                        role.workspace(),
                        tidepool_actor::WorkspaceAccess::InspectOnly
                    );
                    assert_eq!(
                        role.native_tools(),
                        tidepool_actor::NativeToolClass::InspectionOnly
                    );
                    campaign
                        .authority
                        .install_grant(child.actor.identity().into(), worktree_grant(role.role()));
                    let worktree = campaign
                        .worktrees
                        .lookup(&tidepool_worktree::WorktreeId::from_raw(
                            &child.launch_worktrees[0],
                        ))
                        .unwrap()
                        .unwrap();
                    let principal = WorktreePrincipal::exact_actor(
                        &runtime_namespace(campaign.session_root.path()),
                        child.actor.identity().id.0,
                        child.actor.identity().incarnation.0,
                    );
                    assert_eq!(
                        campaign
                            .bindings
                            .lock()
                            .current(worktree.id())
                            .unwrap()
                            .agent(),
                        &principal
                    );
                    let binding = child
                        .worktree_custody
                        .clone()
                        .expect("bootstrap installed custody");
                    child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
                    return (child, binding);
                }
                LocalResidentDeployment::Retired { actor, terminal } => {
                    panic!("{actor:?} retired: {terminal:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("research child admission")
}

#[tokio::test]
async fn research_admission_obeys_configured_width_and_consumes_depth() {
    let mut campaign = TestCampaign::start_with_research_policy(tidepool_actor::ResearchPolicy {
        maximum_depth: 1,
        maximum_active_children: Some(1),
        default_depth: 1,
    })
    .await;
    let root = campaign.root_installation.policy.clone();
    let launch = tokio::spawn(async move {
        dispatch_haskell_script(root.as_ref(), include_str!("research_policy_setup.hs")).await
    });
    let (research, _research_binding) = research_child(&mut campaign).await;
    let result = launch.await.unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    assert_eq!(
        research.effective_role.descendants(),
        tidepool_actor::DescendantBudget {
            maximum_depth: 1,
            maximum_active_children: Some(1)
        }
    );

    let too_wide = dispatch_haskell_script(
        research.policy.as_ref(),
        include_str!("research_policy_width.hs"),
    )
    .await;
    assert_eq!(too_wide["status"], "committed", "{too_wide:?}");
    assert!(
        too_wide["items"].as_array().unwrap().last().unwrap()["output"]
            .as_str()
            .unwrap()
            .eq("True"),
        "{too_wide:?}"
    );

    let escalation = dispatch_haskell_script(
        research.policy.as_ref(),
        include_str!("research_policy_escalation.hs"),
    )
    .await;
    assert_eq!(escalation["status"], "committed", "{escalation:?}");
    assert!(
        escalation["items"].as_array().unwrap().last().unwrap()["output"]
            .as_str()
            .unwrap()
            .eq("True"),
        "{escalation:?}"
    );

    let policy = research.policy.clone();
    let nested = tokio::spawn(async move {
        dispatch_haskell_script(policy.as_ref(), include_str!("research_policy_nested.hs")).await
    });
    let (leaf, _leaf_binding) = research_child(&mut campaign).await;
    let result = nested.await.unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    assert_eq!(leaf.effective_role.descendants().maximum_depth, 0);
    let exhausted = dispatch_haskell_script(
        leaf.policy.as_ref(),
        include_str!("research_policy_exhausted.hs"),
    )
    .await;
    assert_eq!(exhausted["status"], "committed", "{exhausted:?}");
    assert!(exhausted["items"][1]["output"] == "True", "{exhausted:?}");
    let replied = dispatch_haskell_script(leaf.policy.as_ref(), "respond (\"done\" :: Text)").await;
    assert_eq!(replied["status"], "replied", "{replied:?}");
    campaign
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "research policy test complete".into(),
        })
        .await
        .unwrap();
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn preview_and_explicit_research_budget_match_without_spawning_during_preview() {
    let mut campaign = TestCampaign::start_with_research_policy(tidepool_actor::ResearchPolicy {
        default_depth: 1,
        maximum_depth: 3,
        maximum_active_children: Some(4),
    })
    .await;
    let root = campaign.root_installation.policy.clone();
    let preview =
        dispatch_haskell_script(root.as_ref(), include_str!("research_policy_preview.hs")).await;
    assert_eq!(preview["status"], "committed", "{preview:?}");
    assert_eq!(
        preview["items"].as_array().unwrap().last().unwrap()["output"],
        "True",
        "{preview:?}"
    );
    while let Ok(event) = campaign.deployments.try_recv() {
        assert!(
            !matches!(event, LocalResidentDeployment::PolicyInstalled(_)),
            "preview spawned a child"
        );
    }
    let launch = tokio::spawn(async move {
        dispatch_haskell_script(
            root.as_ref(),
            "worker <- unfold (batch campaign group) (child proposal)",
        )
        .await
    });
    let (coordinator, _coordinator_binding) = research_child(&mut campaign).await;
    assert_eq!(launch.await.unwrap()["status"], "committed");
    assert_eq!(
        coordinator.effective_role.descendants(),
        tidepool_actor::DescendantBudget {
            maximum_depth: 2,
            maximum_active_children: Some(2)
        }
    );
    let policy = coordinator.policy.clone();
    let launch = tokio::spawn(async move {
        dispatch_haskell_script(policy.as_ref(), include_str!("research_policy_nested.hs")).await
    });
    let (specialist, _specialist_binding) = research_child(&mut campaign).await;
    assert_eq!(launch.await.unwrap()["status"], "committed");
    assert_eq!(specialist.effective_role.descendants().maximum_depth, 1);
    let policy = specialist.policy.clone();
    let launch = tokio::spawn(async move {
        dispatch_haskell_script(policy.as_ref(), include_str!("research_policy_nested.hs")).await
    });
    let (leaf, _leaf_binding) = research_child(&mut campaign).await;
    assert_eq!(launch.await.unwrap()["status"], "committed");
    assert_eq!(leaf.effective_role.descendants().maximum_depth, 0);
    campaign
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "preview test complete".into(),
        })
        .await
        .unwrap();
    campaign.hosted.await.unwrap();
}
