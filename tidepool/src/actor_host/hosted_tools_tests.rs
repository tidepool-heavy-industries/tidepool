use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use tidepool_tool::{HostedTool, ToolArguments, ToolInvocation};

#[tokio::test]
async fn frozen_tools_dispatch_raw_and_structured_inputs_without_workbench_bindings() {
    let mut campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(), |admission| admission,
        |config| {
            let directory = config.workspace.join(".shoal");
            std::fs::create_dir_all(directory.join("Project")).unwrap();
            std::fs::write(directory.join("Project/Tools.hs"), include_str!("hosted_tools_fixture.hs")).unwrap();
            std::fs::write(directory.join("config.toml"),
                "[defaults]\nmodel='gpt-5.6-sol'\n[haskell]\nsource_roots=['.']\nmodules=['Project.Tools']\ntools='Project.Tools.tools'\n").unwrap();
            config.workspace_inputs = Some(crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root).unwrap());
        },
    ).await;
    let policy = campaign.root_installation.policy.clone();
    // The running dispatcher uses captured source, even if authored files change.
    std::fs::write(
        campaign._repository.path().join(".shoal/Project/Tools.hs"),
        "invalid replacement",
    )
    .unwrap();
    assert!(matches!(&policy.tools()[0], HostedTool::Custom(tool) if tool.name == "haskell"));
    assert!(matches!(&policy.tools()[1], HostedTool::Custom(tool) if tool.name == "raw_echo"));
    assert!(matches!(&policy.tools()[2], HostedTool::Function(tool) if tool.name == "repeat_text"));
    let call = |name: &str, arguments| {
        policy.dispatch_boxed(ToolInvocation {
            context: None,
            name: name.into(),
            arguments,
        })
    };
    for input in ["α\n[not|Haskell|]", "ordinary second call"] {
        let response = call("raw_echo", ToolArguments::Raw(input.into()))
            .await
            .unwrap();
        assert_eq!(response["items"][0]["output"], format!("frozen:{input}"));
        assert!(response["items"][0]["installedBindings"].is_null());
    }
    let response = call(
        "repeat_text",
        ToolArguments::Structured(serde_json::json!({"text":"λ", "copies":3})),
    )
    .await
    .unwrap();
    assert_eq!(response["items"][0]["output"], "λλλ");
    assert!(
        call("raw_echo", ToolArguments::Structured(serde_json::json!({})))
            .await
            .is_err()
    );
    assert!(call("repeat_text", ToolArguments::Raw("x".into()))
        .await
        .is_err());
    assert!(call("unknown", ToolArguments::Raw("x".into()))
        .await
        .is_err());
    let failed = call("raw_echo", ToolArguments::Raw("fail".into()))
        .await
        .unwrap();
    assert_eq!(failed["status"], "rejected", "{failed}");
    let next = call("raw_echo", ToolArguments::Raw("after-failure".into()))
        .await
        .unwrap();
    assert_eq!(next["items"][0]["output"], "frozen:after-failure");
    let huge = call("raw_echo", ToolArguments::Raw("λ".repeat(32000)))
        .await
        .unwrap();
    let text = huge["items"][0]["output"].as_str().unwrap();
    assert!(text.len() <= 32 * 1024, "UTF-8 bytes: {}", text.len());
    let running = tokio::spawn(call(
        "launch",
        ToolArguments::Structured(serde_json::json!({"cmd":"custom", "yield_time_ms":30000})),
    ));
    let backend = super::command_jobs_tests::TestCommands::completed("custom-handler-output");
    super::command_jobs_tests::backend_request(&mut campaign)
        .await
        .supply(Ok(backend));
    let launched = running.await.unwrap().unwrap();
    assert_eq!(launched["status"], "committed", "{launched}");
    let text = launched["items"][0]["output"].as_str().unwrap();
    assert!(
        text.contains("custom-handler-output") && text.contains("handler continued"),
        "{text}"
    );
    let session = text
        .split("session_id: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let observed = call(
        "follow_process",
        ToolArguments::Structured(serde_json::json!({"session_id":session,"yield_time_ms":0})),
    )
    .await
    .unwrap();
    assert_eq!(observed["status"], "committed", "{observed}");
    let page = call(
        "read_log",
        ToolArguments::Structured(serde_json::json!({"session_id":session})),
    )
    .await
    .unwrap();
    assert!(
        page["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("custom-handler-output"),
        "{page}"
    );
    let haskell = dispatch_haskell_script(policy.as_ref(), "40 + 2 :: Int").await;
    assert_eq!(haskell["items"][0]["output"], "42");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
