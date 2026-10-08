//! Retained installed tools and their after-tool slot survive a live spec reload.

use super::test_campaign::{commit_workspace, dispatch_structured_tool, TestCampaign};

fn write_spec(workspace: &std::path::Path, handler: &str, slot: &str) {
    let authored = workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    std::fs::write(
        authored.join("Project/Tools.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/retained_handler_tools.hs",
        )
        .replace("HANDLER_GENERATION", handler),
    )
    .unwrap();
    std::fs::write(
        authored.join("AgentSpec.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/retained_handler_agent_spec.hs",
        )
        .replace("SLOT_GENERATION", slot),
    )
    .unwrap();
}

async fn probe(policy: &dyn exomonad_actor::ResidentToolEndpoint) -> String {
    dispatch_structured_tool(policy, "probe", serde_json::json!({"topic": "anything"}))
        .await
        .to_string()
}

#[tokio::test]
async fn issued_tool_snapshot_keeps_old_handler_after_spec_reload() {
    let before_install = tidepool_extract_cmd::extract_spawn_count();
    let campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            write_spec(&config.workspace, "old-handler", "old slot");
            crate::exomonad::write_fixture_project_config(
                &config.workspace.join(".exomonad"),
                "test-model",
                |project| {
                    project.haskell.source_roots = vec![".".into()];
                    project.haskell.modules = vec!["Project.Tools".into(), "AgentSpec".into()];
                    project.haskell.spec = Some("AgentSpec.agentSpec".into());
                },
            );
            commit_workspace(&config.workspace);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await;
    let after_install = tidepool_extract_cmd::extract_spawn_count();
    assert!(
        after_install > before_install,
        "the accepted AgentSpec installation must submit compiler work"
    );
    let workspace = campaign._repository.path().to_path_buf();
    let policy = campaign.root_installation.policy.clone();
    let old_request = policy
        .snapshot_for_request()
        .expect("initial installed spec");

    let first_old_result = probe(old_request.as_ref()).await;
    assert!(
        first_old_result.contains("old-handler"),
        "{first_old_result}"
    );
    assert!(first_old_result.contains("old slot"), "{first_old_result}");
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        after_install,
        "the first typed tool call must use the installed executable without compiler work"
    );
    let repeated_old_result = probe(old_request.as_ref()).await;
    assert!(
        repeated_old_result.contains("old-handler"),
        "{repeated_old_result}"
    );
    assert!(
        repeated_old_result.contains("old slot"),
        "{repeated_old_result}"
    );
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        after_install,
        "repeated typed tool calls must keep using the installed executable"
    );

    write_spec(&workspace, "new-handler", "new slot");
    let before_reload = tidepool_extract_cmd::extract_spawn_count();
    let receipt = dispatch_structured_tool(
        old_request.as_ref(),
        "reload_agent_spec",
        serde_json::json!({}),
    )
    .await
    .to_string();
    assert!(receipt.contains("swapped"), "{receipt}");
    let new_request = policy
        .snapshot_for_request()
        .expect("reloaded installed spec");
    let after_reload = tidepool_extract_cmd::extract_spawn_count();
    assert!(
        after_reload > before_reload,
        "same-surface reload must submit compiler work before publishing its dispatcher"
    );

    let old_result = probe(old_request.as_ref()).await;
    assert!(old_result.contains("old-handler"), "{old_result}");
    assert!(old_result.contains("old slot"), "{old_result}");
    assert!(!old_result.contains("new-handler"), "{old_result}");
    assert!(!old_result.contains("new slot"), "{old_result}");
    let new_result = probe(new_request.as_ref()).await;
    assert!(new_result.contains("new-handler"), "{new_result}");
    assert!(new_result.contains("new slot"), "{new_result}");
    assert!(!new_result.contains("old-handler"), "{new_result}");
    assert!(!new_result.contains("old slot"), "{new_result}");
    let repeated_new_result = probe(new_request.as_ref()).await;
    assert!(
        repeated_new_result.contains("new-handler"),
        "{repeated_new_result}"
    );
    assert!(
        repeated_new_result.contains("new slot"),
        "{repeated_new_result}"
    );
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        after_reload,
        "first and repeated calls through both retained generations must issue no compiler requests"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
