use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::ResidentToolEndpointTestExt;
use exomonad_tool::{HostedTool, ToolArguments, ToolInvocation};

#[tokio::test]
async fn frozen_tools_dispatch_raw_and_structured_inputs_without_workbench_bindings() {
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let directory = config.workspace.join(".exomonad");
            std::fs::create_dir_all(directory.join("Project")).unwrap();
            std::fs::write(
                directory.join("Project/Tools.hs"),
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/hosted_tools_fixture.hs",
                ),
            )
            .unwrap();
            std::fs::write(
                directory.join("ToolDispatchFixture.hs"),
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/tool_dispatch_fixture.hs",
                ),
            )
            .unwrap();
            crate::exomonad::write_fixture_project_config(&directory, "gpt-6-sol", |project| {
                project.haskell.source_roots = vec![".".into()];
                project.haskell.modules =
                    vec!["Project.Tools".into(), "ToolDispatchFixture".into()];
                project.haskell.spec = Some("Project.Tools.agentSpec".into());
            });
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
    let policy = campaign.root_installation.policy.clone();
    let typed =
        dispatch_haskell_script(policy.as_ref(), "ToolDispatchFixture.dispatchChecks").await;
    assert_eq!(typed["items"][0]["output"], "True", "{typed}");
    // The running dispatcher uses captured source, even if authored files change.
    std::fs::write(
        campaign
            ._repository
            .path()
            .join(".exomonad/Project/Tools.hs"),
        "invalid replacement",
    )
    .unwrap();
    assert!(matches!(&policy.tools()[0], HostedTool::Custom(tool) if tool.name == "haskell"));
    // Resident core tools precede the frozen project tools in declaration order.
    assert!(matches!(&policy.tools()[1], HostedTool::Function(tool) if tool.name == "status"));
    assert!(
        matches!(&policy.tools()[2], HostedTool::Function(tool) if tool.name == "reload_agent_spec")
    );
    assert!(
        matches!(&policy.tools()[3], HostedTool::Function(tool) if tool.name == "reload_helpers")
    );
    assert!(matches!(&policy.tools()[4], HostedTool::Custom(tool) if tool.name == "raw_echo"));
    assert!(matches!(&policy.tools()[5], HostedTool::Function(tool) if tool.name == "repeat_text"));
    let call = |name: &str, arguments| {
        policy.dispatch_json_boxed(ToolInvocation {
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
    for arguments in [
        serde_json::json!({"text": "x", "copies": "three"}),
        serde_json::json!({"text": "x"}),
    ] {
        let error = call("repeat_text", ToolArguments::Structured(arguments))
            .await
            .expect_err("malformed declared input is a tool refusal");
        assert!(
            matches!(
                error,
                exomonad_actor::ResidentToolError::Invocation(
                    exomonad_actor::KernelInvocationFailure::Rejected { .. }
                )
            ),
            "{error}"
        );
    }
    let scalar = call(
        "repeat_text",
        ToolArguments::Structured(serde_json::json!(17)),
    )
    .await
    .expect_err("structured transport requires an object before Haskell dispatch");
    assert!(
        matches!(
            scalar,
            exomonad_actor::ResidentToolError::InvalidInvocation(_)
        ),
        "{scalar}"
    );
    let repaired = call(
        "repeat_text",
        ToolArguments::Structured(serde_json::json!({"text": "fixed", "copies": 1})),
    )
    .await
    .expect("valid call remains usable after malformed input");
    assert_eq!(repaired["items"][0]["output"], "fixed");
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
    let backend = super::command_test_support::TestCommands::completed("custom-handler-output");
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
