//! Releasing a checkpoint prevents new children without revoking admitted custody.

use super::test_campaign::dispatch_haskell_script;
use super::*;
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

#[tokio::test]
async fn released_checkpoint_keeps_an_admitted_childs_hosted_context() {
    let mut campaign =
        test_campaign::TestCampaign::start_with_research_policy(exomonad_actor::ResearchPolicy {
            maximum_depth: 1,
            maximum_active_children: Some(2),
            default_depth: 1,
        })
        .await;
    let root = campaign.root_installation.policy.clone();
    let output_store = display_output::open_run_store(campaign.session_root.path()).unwrap();
    let root_for_setup = root.clone();
    let mut setup = tokio::spawn(async move {
        dispatch_haskell_script(
            root_for_setup.as_ref(),
            include_str!("checkpoint_issuer_setup.hs"),
        )
        .await
    });
    let mut setup_finished = false;
    let issuer = tokio::select! {
        result = &mut setup => {
            let result = result.expect("checkpoint setup task");
            assert_eq!(result["status"], "committed", "checkpoint issuer setup failed: {result:?}");
            setup_finished = true;
            campaign.next_deployment(
                "issuer admitted by completed checkpoint setup", Duration::from_secs(5),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                    other => Err(other),
                },
            ).await
        },
        issuer = campaign.next_deployment(
            "checkpoint issuer", Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        ) => issuer,
    };
    campaign.authority.install_grant(
        issuer.actor.identity().into(),
        worktree_grant(issuer.effective_role.role()),
    );
    issuer.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    if !setup_finished {
        let result = setup.await.unwrap();
        assert_eq!(result["status"], "committed", "{result:?}");
    }

    let capture_call_id = uuid::Uuid::new_v4().simple().to_string();
    let capture = issuer
        .policy
        .dispatch_json_boxed(ToolInvocation {
            context: Some(ToolInvocationContext::external(
                "actor-host-vertical".into(),
                capture_call_id.clone(),
                capture_call_id.clone(),
                Some(capture_call_id.clone()),
                Some("haskell".into()),
            )),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(include_str!("checkpoint_issuer_capture.hs").into()),
        })
        .await
        .unwrap();
    assert_eq!(capture["status"], "committed", "{capture:?}");
    issuer
        .policy
        .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary::external(
            "actor-host-vertical".into(),
            capture_call_id.clone(),
            capture_call_id.clone(),
        ))
        .await
        .unwrap();
    issuer
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "checkpoint issuer retired".into(),
        })
        .await
        .unwrap();

    let issuer_cleanup = issuer
        .actor
        .terminal()
        .cleanup()
        .expect("issuer cleanup evidence");
    assert_eq!(issuer_cleanup.actor(), issuer.actor.identity());
    assert!(issuer_cleanup.is_confirmed(), "{issuer_cleanup:?}");

    let root_for_branch = root.clone();
    let mut branch = tokio::spawn(async move {
        dispatch_haskell_script(
            root_for_branch.as_ref(),
            include_str!("checkpoint_deferred_branch.hs"),
        )
        .await
    });
    let mut branch_finished = false;
    let observer = tokio::select! {
        result = &mut branch => {
            let result = result.unwrap();
            assert_eq!(result["status"], "committed", "{result:?}");
            branch_finished = true;
            campaign.next_deployment(
                "checkpoint observer after completed branch",
                Duration::from_secs(5),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                    other => Err(other),
                },
            ).await
        },
        installation = campaign.next_deployment(
            "checkpoint observer",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        ) => installation,
    };
    assert_eq!(observer.context_parent, Some(issuer.actor.identity()));
    let checkpoint = observer.checkpoint.as_ref().expect("delegated checkpoint");
    let operation = checkpoint.boundary.hosted().expect("hosted checkpoint");
    assert_eq!(operation.external_thread(), Some("actor-host-vertical"));
    assert_eq!(operation.call_id, capture_call_id);
    campaign.authority.install_grant(
        observer.actor.identity().into(),
        worktree_grant(observer.effective_role.role()),
    );
    observer.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    if !branch_finished {
        assert_eq!(branch.await.unwrap()["status"], "committed");
    }

    let release = campaign
        .drive_actor_output(
            &output_store,
            dispatch_haskell_script(root.as_ref(), include_str!("checkpoint_release_twice.hs")),
        )
        .await;
    assert_eq!(release["status"], "committed", "{release:?}");
    assert_eq!(
        test_campaign::committed_display_text(&release),
        "True",
        "{release:?}"
    );
    let refused = campaign
        .drive_actor_output(
            &output_store,
            dispatch_haskell_script(
                root.as_ref(),
                include_str!("checkpoint_released_refusal.hs"),
            ),
        )
        .await;
    assert_eq!(refused["status"], "committed", "{refused:?}");
    assert_eq!(
        test_campaign::committed_display_text(&refused),
        "True",
        "{refused:?}"
    );

    let inherited = campaign
        .drive_actor_output(
            &output_store,
            dispatch_haskell_script(observer.policy.as_ref(), "display (x == 41 && getX == 42)"),
        )
        .await;
    assert_eq!(inherited["status"], "committed", "{inherited:?}");
    assert_eq!(
        test_campaign::committed_display_text(&inherited),
        "True",
        "{inherited:?}"
    );

    let cleaned = campaign
        .drive_actor_output(
            &output_store,
            dispatch_haskell_script(
                root.as_ref(),
                "planCleanupFor observer >>= executeCleanup >>= display . cleanupReceiptComplete",
            ),
        )
        .await;
    assert_eq!(cleaned["status"], "committed", "{cleaned:?}");
    assert_eq!(
        test_campaign::committed_display_text(&cleaned),
        "True",
        "{cleaned:?}"
    );
    let observer_cleanup = observer
        .actor
        .terminal()
        .cleanup()
        .expect("observer cleanup evidence");
    assert_eq!(observer_cleanup.actor(), observer.actor.identity());
    assert!(observer_cleanup.is_confirmed(), "{observer_cleanup:?}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
