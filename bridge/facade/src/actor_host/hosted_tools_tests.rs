use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::ResidentToolEndpointTestExt;
use exomonad_tool::{HostedTool, ToolArguments, ToolInvocation};

#[tokio::test]
async fn frozen_tools_dispatch_raw_and_structured_inputs_without_workbench_bindings() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            let directory = config.workspace.join(".exomonad");
            std::fs::create_dir_all(directory.join("Project")).unwrap();
            std::fs::create_dir_all(directory.join("state")).unwrap();
            std::fs::write(directory.join("state/handler-calls"), b"").unwrap();
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
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let handler_calls = campaign._repository.path().join(".exomonad/state/handler-calls");
    let typed =
        dispatch_haskell_script(policy.as_ref(), "checks <- ToolDispatchFixture.dispatchChecks\nif checks then pure () else error \"dispatch contract failed\"").await;
    assert_eq!(typed["status"], "committed", "{typed}");
    // The running dispatcher uses captured source, even if authored files change.
    std::fs::write(
        campaign
            ._repository
            .path()
            .join(".exomonad/Project/Tools.hs"),
        "invalid replacement",
    )
    .unwrap();
    assert_eq!(policy.tools().iter().map(HostedTool::name).collect::<Vec<_>>(),
        ["status", "haskell", "haskell_sync", "raw_echo", "repeat_text", "launch", "follow_process", "read_log"]);
    assert!(matches!(&policy.tools()[1], HostedTool::Custom(_)));
    assert!(matches!(&policy.tools()[2], HostedTool::Custom(_)));
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
    assert_eq!(std::fs::read(&handler_calls).unwrap(), b"x");
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
        let exomonad_actor::ResidentToolError::Invocation(
            exomonad_actor::KernelInvocationFailure::Workbench(failure),
        ) = error else { panic!("expected authored dispatch input-unit refusal: {error}") };
        assert_eq!(failure.point, tidepool_runtime::session::WorkbenchFailurePoint::InputUnit { index: 0 });
        assert_eq!(failure.total, 1);
        assert!(failure.receipts.is_empty(), "{failure:?}");
        assert_eq!(
            failure.publication,
            Some(tidepool_runtime::session::WorkbenchPublicationOutcome::NotPublished {
                reason: tidepool_runtime::session::WorkbenchNotPublishedReason::Failed,
            }),
            "{failure:?}"
        );
        assert_eq!(std::fs::read(&handler_calls).unwrap(), b"x", "malformed input must not enter the effectful handler");
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
            exomonad_actor::ResidentToolError::Declaration(
                exomonad_tool::ToolDeclarationError::InvalidArguments { ref name }
            ) if name == "repeat_text"
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
    assert_eq!(std::fs::read(&handler_calls).unwrap(), b"xx");
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
        .expect_err("oversized semantic output cannot publish a partial dispatcher envelope");
    let exomonad_actor::ResidentToolError::Invocation(
        exomonad_actor::KernelInvocationFailure::Workbench(huge_failure),
    ) = huge else { panic!("expected output observation workbench failure: {huge}") };
    assert_eq!(huge_failure.point, tidepool_runtime::session::WorkbenchFailurePoint::InputUnit { index: 0 });
    assert_eq!(huge_failure.total, 1);
    assert!(huge_failure.receipts.is_empty(), "{huge_failure:?}");
    assert_eq!(huge_failure.publication,
        Some(tidepool_runtime::session::WorkbenchPublicationOutcome::NotPublished {
            reason: tidepool_runtime::session::WorkbenchNotPublishedReason::Failed,
        }), "{huge_failure:?}");
    let unicode = "λ".repeat(256);
    let recovered = call("raw_echo", ToolArguments::Raw(unicode.clone())).await.unwrap();
    assert_eq!(recovered["status"], "committed", "{recovered}");
    assert_eq!(recovered["items"][0]["output"], format!("frozen:{unicode}"));
    assert_eq!(recovered["items"][0]["value"], format!("frozen:{unicode}"));
    let running = tokio::spawn(call(
        "launch",
        ToolArguments::Structured(serde_json::json!({"cmd":"custom", "yield_time_ms":30000})),
    ));
    let backend = super::command_test_support::TestCommands::completed("custom-handler-output");
    super::command_jobs_tests::backend_request(campaign)
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
    let haskell = dispatch_haskell_script(policy.as_ref(), "answer <- pure (40 + 2 :: Int)\nif answer == 42 then pure () else error \"notebook contract failed\"").await;
    assert_eq!(haskell["status"], "committed", "{haskell}");
})).await;
}
