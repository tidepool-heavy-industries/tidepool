use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;
use tidepool_actor::command_jobs::{CommandBackend, CommandControl};
use tidepool_bridge_effects::*;

struct TestCommands {
    specs: Mutex<Vec<CommandSpec>>,
    finish: watch::Sender<bool>,
    cancelled: std::sync::atomic::AtomicBool,
}
impl TestCommands {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            specs: Mutex::new(Vec::new()),
            finish: watch::channel(false).0,
            cancelled: false.into(),
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
    fn output<'a>(
        &'a self,
        _id: &'a str,
        _bytes: usize,
    ) -> futures_util::future::BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async {
            Ok(CommandOutput {
                stdout: "result".into(),
                stderr: String::new(),
                truncated: false,
            })
        })
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
        "Cmd.await job\nCmd.output job 1024\nR.call (completionCount (R.client listener)) ()",
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
    assert!(result.to_string().contains("commandStdout"), "{result}");
    assert_eq!(backend.specs.lock()[0].memory, 4 * 1024 * 1024 * 1024);
    let description = committed(&campaign, examples.next().unwrap()).await;
    assert!(
        description.to_string().contains("a path; not shell syntax"),
        "{description}"
    );
    assert!(
        examples.next().is_none(),
        "new skill examples need execution coverage"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
