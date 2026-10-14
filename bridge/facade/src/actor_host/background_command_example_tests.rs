use super::command_jobs_tests::{backend_request_while_live, committed};
use super::command_test_support::TestCommands;
use super::test_campaign::{TestCampaign, COLD_DEBUG_CELL_SETTLEMENT_BUDGET};
use std::sync::Arc;

fn example_stage(name: &str) -> String {
    let example = tidepool_testing::fixture_source(
        "exomonad/examples/workspace/.exomonad/checks/background-command-example.hs",
    );
    let marker = format!("-- Stage: {name}\n");
    let (_, after) = example
        .split_once(&marker)
        .unwrap_or_else(|| panic!("missing {marker:?} in background command example"));
    after
        .split_once("\n-- Stage: ")
        .map_or(after, |(stage, _)| stage)
        .trim()
        .to_owned()
}

async fn workspace_campaign() -> TestCampaign {
    let reader: exomonad_actor::ConversationReader = Arc::new(|_actor, _count| {
        Box::pin(async { Err(exomonad_actor::ConversationUnavailable::Unbound) })
    });
    TestCampaign::start_with_conversation(
        |admission| admission,
        super::jev_tests::pinned_jev_workspace,
        Some(reader),
    )
    .await
}

async fn completion_case(
    campaign: &mut TestCampaign,
    exit_code: i64,
    output_unavailable: bool,
) -> String {
    committed(campaign, &example_stage("imports")).await;
    let policy = campaign.root_installation.policy.clone();
    let mut awaiting = tokio::spawn(async move {
        super::tests::dispatch_haskell_script(policy.as_ref(), &example_stage("await")).await
    });
    let backend = TestCommands::new();
    backend.set_exit_code(exit_code);
    if output_unavailable {
        backend.set_output_unavailable();
    }
    backend_request_while_live(campaign, &mut awaiting, COLD_DEBUG_CELL_SETTLEMENT_BUDGET)
        .await
        .supply(Ok(backend.clone()));
    backend.finish();
    let awaited = awaiting.await.unwrap();
    assert_eq!(awaited["status"], "committed", "{awaited}");

    let projection = committed(
        campaign,
        &format!("display (tshow ({}))", example_stage("compact-query")),
    )
    .await;
    let projection_text = super::test_campaign::committed_display_text(&projection);
    assert!(
        projection_text.contains("CompletionProjection"),
        "{projection}"
    );
    let evidence = committed(
        campaign,
        &format!("display (tshow ({}))", example_stage("full-evidence")),
    )
    .await;
    let evidence_text = super::test_campaign::committed_display_text(&evidence);
    assert!(evidence_text.contains("CompletionEvidence"), "{evidence}");
    assert_eq!(
        backend.executions(),
        1,
        "the original job must be the only execution"
    );
    format!("{projection_text} {evidence_text}")
}

#[tokio::test]
async fn background_command_example_retains_success_failure_and_unavailable_capture() {
    let campaign = workspace_campaign().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                committed(
                    campaign,
                    "import qualified Project.BackgroundCommandExampleChecks",
                )
                .await;
                let success = completion_case(campaign, 0, false).await;
                assert!(success.contains("CommandExited 0"), "{success}");
                assert!(success.contains("CaptureComplete"), "{success}");
                assert!(success.contains(r#"CaptureComplete "result""#), "{success}");

                let failure = completion_case(campaign, 7, false).await;
                assert!(failure.contains("CommandExited 7"), "{failure}");
                assert!(failure.contains("CaptureComplete"), "{failure}");

                let unavailable = completion_case(campaign, 0, true).await;
                assert!(unavailable.contains("CommandExited 0"), "{unavailable}");
                assert!(unavailable.contains("RefusedCapture"), "{unavailable}");
                assert!(unavailable.contains("CommandUnavailable"), "{unavailable}");
                assert!(
                    unavailable.contains("output transport lost"),
                    "{unavailable}"
                );
            })
        })
        .await;
}
