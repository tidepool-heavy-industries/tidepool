use super::command_jobs_tests::{backend_request, committed};
use super::command_test_support::TestCommands;
use super::test_campaign::TestCampaign;

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
    TestCampaign::start_with_config(
        |admission| admission,
        super::jev_tests::pinned_jev_workspace,
    )
    .await
}

async fn completion_case(
    campaign: &mut TestCampaign,
    exit_code: i64,
    output_unavailable: bool,
) -> String {
    let imports = committed(campaign, &example_stage("imports")).await;
    assert_eq!(imports["status"], "committed", "{imports}");
    let started = committed(campaign, &example_stage("start")).await;
    assert_eq!(started["status"], "committed", "{started}");

    let backend = TestCommands::new();
    backend.set_exit_code(exit_code);
    if output_unavailable {
        backend.set_output_unavailable();
    }
    backend_request(campaign).await.supply(Ok(backend.clone()));
    backend.finish();

    // This later cell waits for the existing job without reading its output.
    // The watcher then returns the compact view and the original full capture.
    let status = committed(campaign, "Cmd.observe (Cmd.Observation 30000 0) job").await;
    assert!(
        status
            .to_string()
            .contains(&format!("CommandExited {exit_code}")),
        "{status}"
    );
    // Attaching after terminal settlement exercises retained Cmd.completion:
    // the source queues its exact receipt before actor startup returns.
    let attached = committed(campaign, &example_stage("watch")).await;
    assert_eq!(attached["status"], "committed", "{attached}");
    let projection = committed(campaign, &example_stage("compact-query")).await;
    assert!(
        projection.to_string().contains("CompletionProjection"),
        "{projection}"
    );
    let evidence = committed(campaign, &example_stage("full-evidence")).await;
    assert!(
        evidence.to_string().contains("CompletionEvidence"),
        "{evidence}"
    );
    assert_eq!(
        backend.executions(),
        1,
        "the original job must be the only execution"
    );
    let finished = committed(campaign, &example_stage("cleanup")).await;
    assert_eq!(finished["status"], "committed", "{finished}");
    format!("{} {} {}", projection, evidence, finished)
}

#[tokio::test]
async fn background_command_example_retains_success_failure_and_unavailable_capture() {
    let mut campaign = workspace_campaign().await;
    committed(
        &campaign,
        "import qualified Project.BackgroundCommandExampleChecks",
    )
    .await;
    let success = completion_case(&mut campaign, 0, false).await;
    assert!(success.contains("CommandExited 0"), "{success}");
    assert!(success.contains("CaptureComplete"), "{success}");
    assert!(
        success.contains(r#"CaptureComplete \"result\""#),
        "{success}"
    );

    let failure = completion_case(&mut campaign, 7, false).await;
    assert!(failure.contains("CommandExited 7"), "{failure}");
    assert!(failure.contains("CaptureComplete"), "{failure}");

    let unavailable = completion_case(&mut campaign, 0, true).await;
    assert!(unavailable.contains("CommandExited 0"), "{unavailable}");
    assert!(unavailable.contains("RefusedCapture"), "{unavailable}");
    assert!(unavailable.contains("CommandUnavailable"), "{unavailable}");
    assert!(
        unavailable.contains("output transport lost"),
        "{unavailable}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
