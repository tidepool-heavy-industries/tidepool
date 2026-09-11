use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use tidepool_tool::{HostedTool, ToolArguments, ToolInvocation};

#[tokio::test]
async fn frozen_tools_dispatch_raw_and_structured_inputs_without_workbench_bindings() {
    let campaign = TestCampaign::start_with_config(
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
    let policy = &campaign.root_installation.policy;
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
    let haskell = dispatch_haskell_script(policy.as_ref(), "40 + 2 :: Int").await;
    assert_eq!(haskell["items"][0]["output"], "42");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
