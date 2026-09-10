use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;
use tidepool_actor::command_jobs::{CommandBackend, CommandControl};
use tidepool_bridge_effects::*;

struct TestCommands {
    specs: Mutex<Vec<CommandSpec>>,
    finish: watch::Sender<bool>,
    cancelled: std::sync::atomic::AtomicBool,
    output_unavailable: std::sync::atomic::AtomicBool,
}
impl TestCommands {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            specs: Mutex::new(Vec::new()),
            finish: watch::channel(false).0,
            cancelled: false.into(),
            output_unavailable: false.into(),
        })
    }
}
impl CommandBackend for TestCommands {
    fn cleanup<'a>(&'a self, _id: &'a str) -> futures_util::future::BoxFuture<'a, CommandCleanup> {
        Box::pin(async { CommandCleanup::CommandClean })
    }
    fn execute<'a>(
        &'a self,
        _id: &'a str,
        spec: CommandSpec,
        _phase: watch::Sender<CommandStatus>,
    ) -> futures_util::future::BoxFuture<'a, CommandResult> {
        Box::pin(async move {
            self.specs.lock().push(spec);
            let mut done = self.finish.subscribe();
            while !*done.borrow_and_update() {
                done.changed().await.unwrap();
            }
            CommandResult {
                outcome: if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    CommandOutcome::CommandCancelled
                } else {
                    CommandOutcome::CommandExited(0)
                },
                cleanup: CommandCleanup::CommandClean,
            }
        })
    }
    fn control<'a>(
        &'a self,
        _id: &'a str,
        operation: CommandControl,
    ) -> futures_util::future::BoxFuture<'a, Result<(), CommandError>> {
        Box::pin(async move {
            if matches!(operation, CommandControl::Cancel) {
                self.cancelled
                    .store(true, std::sync::atomic::Ordering::Release);
                self.finish.send_replace(true);
            }
            Ok(())
        })
    }
    fn read<'a>(
        &'a self,
        _id: &'a str,
        stream: CommandStream,
        _position: CommandPosition,
    ) -> futures_util::future::BoxFuture<'a, Result<CommandPage, CommandError>> {
        Box::pin(async move {
            Ok(test_page(match stream {
                CommandStream::Stdout => "result",
                CommandStream::Stderr => "",
            }))
        })
    }
    fn output<'a>(
        &'a self,
        _id: &'a str,
        _bytes: usize,
    ) -> futures_util::future::BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async {
            if self
                .output_unavailable
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(CommandError::CommandUnavailable(
                    "output transport lost".into(),
                ));
            }
            Ok(CommandOutput {
                stdout: test_page("result"),
                stderr: test_page(""),
            })
        })
    }
}

fn test_page(text: &str) -> CommandPage {
    CommandPage {
        text: text.into(),
        start: 0,
        end: text.len() as i64,
        available_end: text.len() as i64,
        retained_start: 0,
        lost_bytes: 0,
        finished: true,
        lossy: false,
        leading_fragment: false,
        trailing_fragment: false,
    }
}

async fn committed(campaign: &TestCampaign, source: &str) -> serde_json::Value {
    let result = dispatch_haskell_script(campaign.root_installation.policy.as_ref(), source).await;
    assert_eq!(result["status"], "committed", "{result}");
    for item in result["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{result}");
    }
    result
}
async fn backend_request(
    campaign: &mut TestCampaign,
) -> Arc<tidepool_actor::command_jobs::CommandBackendRequest> {
    loop {
        if let LocalResidentDeployment::CommandBackend(request) =
            campaign.deployments.recv().await.unwrap()
        {
            return request;
        }
    }
}

#[tokio::test]
async fn command_jobs_retain_completion_and_route_to_record_actors() {
    let mut campaign = TestCampaign::start().await;
    let initial = committed(&campaign, include_str!("command_jobs.hs")).await;
    assert!(initial.to_string().contains("CommandQueued"), "{initial}");
    let backend = TestCommands::new();
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    backend.finish.send_replace(true);
    let result = committed(
        &campaign,
        "Cmd.await job\nCmd.readOutput Cmd.Stdout job\nR.call (completionCount (R.client listener)) ()",
    )
    .await;
    assert!(result.to_string().contains("CommandExited 0"), "{result}");
    assert_eq!(
        backend.specs.lock()[0].argv,
        [
            "bash",
            "--noprofile",
            "--norc",
            "-c",
            "printf '%s' \"$1\"",
            "shoal-bash",
            "a b;$HOME\n'quoted'"
        ]
    );
    assert_eq!(backend.specs.lock()[0].memory, 8 * 1024 * 1024 * 1024);
    assert_eq!(
        backend.specs.lock()[0].environment,
        [("A".into(), "new".into()), ("B".into(), "kept".into())]
    );
    // Completion may already have been published when a source is installed.
    let late = committed(
        &campaign,
        "late <- R.start collector\nR.call (completionCount (R.client late)) ()",
    )
    .await;
    assert_eq!(late["items"][1]["output"], "1", "{late}");
    let early = committed(
        &campaign,
        "R.call (completionCount (R.client listener)) ()\nR.finish listener\nR.finish late",
    )
    .await;
    assert_eq!(early["items"][0]["output"], "1", "{early}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn command_output_ux_preserves_large_values_and_decodes_complete_stdout() {
    let mut campaign = TestCampaign::start().await;
    committed(&campaign, "job <- Cmd.start [bash|printf result|]").await;
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    backend_request(&mut campaign).await.supply(Ok(backend));
    committed(&campaign, "finished <- Cmd.await job").await;
    let result = committed(&campaign, include_str!("command_output_ux.hs")).await;
    let text = result.to_string();
    for marker in [
        "large-display-ok",
        "json-ok",
        "partial-rejected",
        "streams-independent",
        "decode-error-distinct",
        "omission-kinds-preserved",
        "data RunResult",
    ] {
        assert!(text.contains(marker), "missing {marker}: {text}");
    }
    assert!(!text.contains("Display failed"), "{text}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn failed_command_display_retains_result_without_reexecution() {
    let mut campaign = TestCampaign::start().await;
    committed(&campaign, "job <- Cmd.start [bash|printf result|]").await;
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    committed(&campaign, "finished <- Cmd.await job").await;
    let failed = committed(&campaign, include_str!("command_display_failure.hs")).await;
    let text = failed.to_string();
    assert!(text.contains("Display failed"), "{text}");
    assert!(text.contains("Value remains bound"), "{text}");
    assert!(!text.contains("Expand: inspectFull"), "{text}");
    let recovered = committed(&campaign, "Cmd.stdout (savedResult broken)").await;
    assert!(
        recovered.to_string().contains("Right result"),
        "{recovered}"
    );
    assert_eq!(backend.specs.lock().len(), 1);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn command_jobs_cancel_before_backend_cannot_start_later() {
    let mut campaign = TestCampaign::start().await;
    committed(
        &campaign,
        "job <- Cmd.start [bash|never-execute|]\nCmd.cancel job\nCmd.await job",
    )
    .await;
    let backend = TestCommands::new();
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let result = committed(&campaign, "Cmd.status job").await;
    assert!(result.to_string().contains("CommandCancelled"), "{result}");
    assert!(backend.specs.lock().is_empty());
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn command_run_retains_job_after_output_observation_failure() {
    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();
    let running = tokio::spawn(async move {
        dispatch_haskell_script(policy.as_ref(), "attempt <- Cmd.run [bash|printf done|]").await
    });
    let backend = TestCommands::new();
    backend
        .output_unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    backend.finish.send_replace(true);
    backend_request(&mut campaign).await.supply(Ok(backend));
    let result = running.await.unwrap();
    assert_eq!(result["status"], "committed", "{result}");
    let retained = committed(
        &campaign,
        "let retained = Cmd.job attempt\nCmd.status retained",
    )
    .await;
    assert!(
        retained.to_string().contains("CommandExited 0"),
        "{retained}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn command_skill_examples_execute_in_the_resident_workbench() {
    let mut campaign = TestCampaign::start().await;
    let skill =
        include_str!("../../../examples/shoal-workspace/.shoal/skills/shoal-command/SKILL.md");
    let mut examples = skill
        .split("```haskell\n")
        .skip(1)
        .map(|block| block.split_once("```").unwrap().0);
    committed(&campaign, examples.next().unwrap()).await;
    let backend = TestCommands::new();
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    backend.finish.send_replace(true);
    let result = committed(&campaign, examples.next().unwrap()).await;
    assert!(result.to_string().contains("result"), "{result}");
    assert_eq!(backend.specs.lock()[0].memory, 256 * 1024 * 1024);
    assert!(backend.specs.lock()[0].environment.is_empty());
    let description = committed(&campaign, examples.next().unwrap()).await;
    assert!(
        description.to_string().contains("a path; not shell syntax"),
        "{description}"
    );
    committed(&campaign, examples.next().unwrap()).await;
    assert!(
        examples.next().is_none(),
        "new skill examples need execution coverage"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
#[ignore = "manual resident declaration latency probe; no subprocess execution"]
async fn command_description_latency_probe() {
    let campaign = TestCampaign::start().await;
    for (label, source) in [
        ("argv-first", "let a = Cmd.argv [\"printf\", \"one\"]"),
        ("quote-first", "let b = [bash|printf two|]"),
        ("quote-second", "let c = [bash|printf three|]"),
        ("argv-second", "let d = Cmd.argv [\"printf\", \"four\"]"),
        ("reuse", "Cmd.describe b"),
    ] {
        let start = std::time::Instant::now();
        committed(&campaign, source).await;
        eprintln!(
            "command-description {label} elapsed_ms={}",
            start.elapsed().as_millis()
        );
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
