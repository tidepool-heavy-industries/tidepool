use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;
use tidepool_actor::command_jobs::{CommandBackend, CommandControl};
use tidepool_bridge_effects::*;
use tidepool_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

pub(super) struct TestCommands {
    specs: Mutex<Vec<CommandSpec>>,
    stdout: Mutex<String>,
    finish: watch::Sender<bool>,
    cancelled: std::sync::atomic::AtomicBool,
    output_unavailable: std::sync::atomic::AtomicBool,
    output_entered: tokio::sync::Notify,
    hold_output: watch::Sender<bool>,
}
impl TestCommands {
    pub(super) fn completed(stdout: &str) -> Arc<Self> {
        let backend = Self::new();
        *backend.stdout.lock() = stdout.into();
        backend.finish.send_replace(true);
        backend
    }

    fn new() -> Arc<Self> {
        Arc::new(Self {
            specs: Mutex::new(Vec::new()),
            stdout: Mutex::new("result".into()),
            finish: watch::channel(false).0,
            cancelled: false.into(),
            output_unavailable: false.into(),
            output_entered: tokio::sync::Notify::new(),
            hold_output: watch::channel(false).0,
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
        position: CommandPosition,
    ) -> futures_util::future::BoxFuture<'a, Result<CommandPage, CommandError>> {
        Box::pin(async move {
            let mut page = match stream {
                CommandStream::Stdout => test_page(&self.stdout.lock()),
                CommandStream::Stderr => test_page(""),
            };
            if let CommandPosition::OutputOffset(offset) = position {
                let start = offset.min(page.end).max(0);
                page.text = page.text[start as usize..].to_owned();
                page.start = start;
            }
            Ok(page)
        })
    }
    fn output<'a>(
        &'a self,
        _id: &'a str,
        _bytes: usize,
    ) -> futures_util::future::BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async {
            self.output_entered.notify_one();
            let mut held = self.hold_output.subscribe();
            while *held.borrow_and_update() {
                held.changed().await.unwrap();
            }
            if self
                .output_unavailable
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(CommandError::CommandUnavailable(
                    "output transport lost".into(),
                ));
            }
            Ok(CommandOutput {
                stdout: test_page(&self.stdout.lock()),
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
pub(super) async fn backend_request(
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
async fn raw_bash_uses_compiled_handler_and_shared_command_owner() {
    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();
    assert!(policy.tools().iter().any(|tool| matches!(tool,
        tidepool_tool::HostedTool::Custom(declaration) if declaration.name == "bash")));
    let script = "cat <<'EOF'\nλ; $(literal) [bash|text|]\nEOF\n";
    let invocation = ToolInvocation {
        name: "bash".into(),
        arguments: ToolArguments::Raw(script.into()),
        context: Some(ToolInvocationContext {
            context_call_id: Some("raw-once".into()),
            thread_id: "raw-thread".into(),
            turn_id: "raw-turn".into(),
            call_id: "raw-once".into(),
            namespace: Some("tidepool_actor".into()),
        }),
    };
    let first = tokio::spawn(policy.dispatch_boxed(invocation.clone()));
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let receipt = first.await.unwrap().unwrap();
    assert_eq!(receipt["status"], "committed", "{receipt}");
    assert!(
        receipt["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("result"),
        "{receipt}"
    );
    assert!(
        receipt["items"][0]["installedBindings"].is_null(),
        "{receipt}"
    );
    assert_eq!(
        policy.dispatch_boxed(invocation.clone()).await.unwrap(),
        receipt
    );
    assert_eq!(
        backend.specs.lock().len(),
        1,
        "replayed call must not execute twice"
    );
    assert_eq!(
        backend.specs.lock()[0].argv[4],
        script,
        "script is data, including Haskell delimiters"
    );
    assert_eq!(backend.specs.lock()[0].memory, 256 * 1024 * 1024);
    let mut changed = invocation;
    changed.arguments = ToolArguments::Raw("changed".into());
    assert!(policy.dispatch_boxed(changed).await.is_err());
    committed(&campaign, "40 + 2 :: Int").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn raw_bash_oversized_output_retains_a_real_job() {
    let mut campaign = TestCampaign::start().await;
    let backend = TestCommands::new();
    *backend.stdout.lock() = format!("BEGIN\n{}\nEND\n", "λ".repeat(32_000));
    backend.finish.send_replace(true);
    let policy = campaign.root_installation.policy.clone();
    let running = tokio::spawn(policy.dispatch_boxed(ToolInvocation {
        context: None,
        name: "bash".into(),
        arguments: ToolArguments::Raw("large".into()),
    }));
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let response = running.await.unwrap().unwrap();
    assert_eq!(response["status"], "committed", "{response}");
    let output = response["items"][0]["output"].as_str().unwrap();
    assert!(output.len() < 10 * 1024, "{}", output.len());
    assert!(
        output.contains("BEGIN") && output.contains("END"),
        "{output}"
    );
    let binding = response["items"][0]["installedBindings"][0]
        .as_str()
        .unwrap();
    assert!(
        output.contains(&format!("{binding} :: Cmd.Job")),
        "{output}"
    );
    let observed = committed(
        &campaign,
        &format!("saved <- Cmd.readStdout {binding}\nfmap T.length saved"),
    )
    .await;
    assert!(observed.to_string().contains("32011"), "{observed}");
    assert_eq!(backend.specs.lock().len(), 1);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn raw_bash_timeout_preserves_the_command_for_haskell_continuation() {
    let mut campaign = TestCampaign::start().await;
    let backend = TestCommands::new();
    let policy = campaign.root_installation.policy.clone();
    let running = tokio::spawn(policy.dispatch_boxed(ToolInvocation {
        context: None,
        name: "bash".into(),
        arguments: ToolArguments::Raw("long-lived".into()),
    }));
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let response = running.await.unwrap().unwrap();
    assert_eq!(response["status"], "backgrounded", "{response}");
    let binding = response["items"][0]["installedBindings"][0]
        .as_str()
        .unwrap();
    assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
    backend.finish.send_replace(true);
    let observed = committed(
        &campaign,
        &format!("saved <- Cmd.await {binding}\nCmd.stdout saved"),
    )
    .await;
    assert!(observed.to_string().contains("result"), "{observed}");
    assert_eq!(backend.specs.lock().len(), 1);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
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
async fn command_hidden_by_the_response_budget_remains_unobserved() {
    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();
    let running = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            "inspectFull (T.replicate 70000 \"x\")\nhidden <- Cmd.run [bash|printf ignored|]",
        )
        .await
    });
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let first = running.await.unwrap();
    assert_eq!(first["status"], "committed");
    assert!(!first.to_string().contains("stdout ·"));
    let next = committed(&campaign, "Cmd.await (Cmd.job hidden)").await;
    assert!(next.to_string().contains("stdout ·"), "{next}");
    assert!(next.to_string().contains("result"), "{next}");
    assert_eq!(backend.specs.lock().len(), 1);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn disconnected_foreground_caller_retries_the_same_handoff_without_reexecution() {
    let mut campaign = TestCampaign::start().await;
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    backend.hold_output.send_replace(true);
    backend
        .output_unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    let call_id = uuid::Uuid::new_v4().to_string();
    let invocation = || ToolInvocation {
        context: Some(ToolInvocationContext {
            context_call_id: Some(call_id.clone()),
            thread_id: "disconnected-foreground".into(),
            turn_id: call_id.clone(),
            call_id: call_id.clone(),
            namespace: Some("haskell".into()),
        }),
        name: tidepool_actor::HASKELL_TOOL.into(),
        arguments: ToolArguments::Raw(include_str!("command_foreground_stop.hs").into()),
    };
    let policy = campaign.root_installation.policy.clone();
    let first = invocation();
    let waiter = tokio::spawn(async move { policy.dispatch_boxed(first).await });
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    backend.output_entered.notified().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    backend.hold_output.send_replace(false);
    let policy = campaign.root_installation.policy.as_ref();
    let recovered = policy.dispatch_boxed(invocation()).await.unwrap();
    let repeated = policy.dispatch_boxed(invocation()).await.unwrap();
    assert_eq!(
        recovered, repeated,
        "exact transport retry changed the receipt"
    );
    assert_eq!(recovered["status"], "backgrounded", "{recovered}");
    let binding = recovered["items"][1]["installedBindings"][0]
        .as_str()
        .unwrap();
    policy
        .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary {
            thread_id: "disconnected-foreground".into(),
            call_id,
        })
        .await
        .unwrap();
    let usable = committed(&campaign, &format!("Cmd.status {binding}")).await;
    assert!(usable.to_string().contains("CommandExited 0"), "{usable}");
    assert_eq!(backend.specs.lock().len(), 1);
    assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn command_run_retains_job_after_output_observation_failure() {
    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();
    let running = tokio::spawn(async move {
        dispatch_haskell_script(policy.as_ref(), include_str!("command_foreground_stop.hs")).await
    });
    let backend = TestCommands::new();
    backend
        .output_unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    backend.finish.send_replace(true);
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let result = running.await.unwrap();
    assert_eq!(result["status"], "backgrounded", "{result}");
    assert_eq!(result["items"][0]["status"], "committed", "{result}");
    assert_eq!(result["items"][1]["status"], "stopped", "{result}");
    assert_eq!(result["items"][2]["status"], "notRun", "{result}");
    let binding = result["items"][1]["installedBindings"][0].as_str().unwrap();
    assert_eq!(
        backend.specs.lock().len(),
        1,
        "abandoned continuation ran another command"
    );
    let retained = committed(&campaign, &format!("Cmd.status {binding}")).await;
    assert!(
        retained.to_string().contains("CommandExited 0"),
        "{retained}"
    );
    let missing = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        "Cmd.job attempt",
    )
    .await;
    assert!(
        missing.to_string().contains("not in scope")
            || missing.to_string().contains("Not in scope"),
        "{missing}"
    );
    let prefix = committed(&campaign, "foregroundPrefix").await;
    assert!(prefix.to_string().contains("prefix-preserved"), "{prefix}");
    let repeated = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        &format!("Cmd.await {binding}"),
    )
    .await;
    assert_eq!(
        repeated["items"][0]["installedBindings"][0], binding,
        "{repeated}"
    );
    committed(
        &campaign,
        &format!("let savedCommandJob = {binding}\n{binding} <- pure (17 :: Int)"),
    )
    .await;
    let shadowed = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        "Cmd.await savedCommandJob",
    )
    .await;
    let fresh = shadowed["items"][0]["installedBindings"][0]
        .as_str()
        .unwrap();
    assert_ne!(
        fresh, binding,
        "shadowed automatic alias was reused: {shadowed}"
    );
    let preserved = committed(&campaign, binding).await;
    assert!(preserved.to_string().contains("17"), "{preserved}");
    let binding = fresh;
    backend
        .output_unavailable
        .store(false, std::sync::atomic::Ordering::Release);
    let recovered = committed(
        &campaign,
        &format!("recovered <- Cmd.await {binding}\nCmd.stdout recovered"),
    )
    .await;
    assert!(recovered.to_string().contains("result"), "{recovered}");
    assert_eq!(backend.specs.lock().len(), 1, "recovery reran the command");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn command_foreground_deadline_installs_binding_without_cancelling_or_resuming() {
    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();
    let running = tokio::spawn(async move {
        dispatch_haskell_script(policy.as_ref(), include_str!("command_foreground_stop.hs")).await
    });
    let backend = TestCommands::new();
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let result = running.await.unwrap();
    assert_eq!(result["status"], "backgrounded", "{result}");
    assert_eq!(result["items"][1]["status"], "stopped", "{result}");
    assert_eq!(result["items"][2]["status"], "notRun", "{result}");
    assert!(
        result["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains("stdout ·"),
        "handoff omitted available output: {result}"
    );
    let binding = result["items"][1]["installedBindings"][0].as_str().unwrap();
    let live = committed(&campaign, &format!("Cmd.status {binding}")).await;
    assert!(!live.to_string().contains("CommandFinished"), "{live}");
    assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(backend.specs.lock().len(), 1);
    backend.finish.send_replace(true);
    let recovered = committed(
        &campaign,
        &format!("recovered <- Cmd.await {binding}\nCmd.stdout recovered"),
    )
    .await;
    assert!(recovered.to_string().contains("result"), "{recovered}");
    assert_eq!(
        backend.specs.lock().len(),
        1,
        "await resumed the abandoned continuation"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn command_handler_failure_does_not_install_interactive_recovery_binding() {
    let mut campaign = TestCampaign::start().await;
    committed(&campaign, include_str!("command_handler_stop.hs")).await;
    let policy = campaign.root_installation.policy.clone();
    let mut running = tokio::spawn(async move {
        super::tests::dispatch_haskell_script_result(
            policy.as_ref(),
            "R.call (execute (R.client handler)) ()",
        )
        .await
    });
    let backend = TestCommands::new();
    backend
        .output_unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    backend.finish.send_replace(true);
    let request = tokio::select! {
        result = &mut running => panic!("handler ended before requesting backend: {result:?}"),
        request = backend_request(&mut campaign) => request,
    };
    request.supply(Ok(backend.clone()));
    let result = running
        .await
        .unwrap()
        .expect_err("handler failure must propagate")
        .to_string();
    assert!(result.contains("output transport lost"), "{result}");
    assert!(!result.contains("installedBindings"), "{result}");
    assert!(!result.contains("Continue with:"), "{result}");
    assert_eq!(backend.specs.lock().len(), 1);
    committed(&campaign, "21 + 21 :: Int").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn command_presentation_is_automatic_scoped_and_retains_quiet_results() {
    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();
    let mut running = tokio::spawn(async move {
        dispatch_haskell_script(policy.as_ref(), include_str!("command_presentation.hs")).await
    });
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    for _ in 0..3 {
        let request = tokio::select! {
            result = &mut running => panic!("command program ended before its expected launches: {result:?}"),
            request = backend_request(&mut campaign) => request,
        };
        request.supply(Ok(backend.clone()));
    }
    let result = running.await.unwrap();
    assert_eq!(result["status"], "committed", "{result}");
    let text = |index: usize| result["items"][index]["output"].as_str().unwrap();
    assert_eq!(text(0).matches("stdout ·").count(), 1, "{result}");
    assert!(!text(1).contains("stdout ·"), "{result}");
    assert_eq!(text(2).matches("stdout ·").count(), 1, "{result}");
    assert!(text(3).contains("Right result"), "{result}");
    assert!(
        !text(4).contains("stdout ·"),
        "repeated await repeated output: {result}"
    );
    assert!(
        text(5).contains("result"),
        "explicit output was consumed: {result}"
    );
    assert_eq!(backend.specs.lock().len(), 3);
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
    let policy = campaign.root_installation.policy.clone();
    let first = examples.next().unwrap().to_owned();
    let mut running =
        tokio::spawn(async move { dispatch_haskell_script(policy.as_ref(), &first).await });
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    tokio::select! {
        request = backend_request(&mut campaign) => request.supply(Ok(backend.clone())),
        result = &mut running => panic!("skill failed before launching: {result:?}"),
    }
    let first = running.await.unwrap();
    assert_eq!(first["status"], "committed", "{first}");
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

#[tokio::test]
async fn command_binding_failure_preserves_the_existing_job_without_claiming_an_alias() {
    let mut campaign = TestCampaign::start().await;
    committed(
        &campaign,
        "retainedBeforeFailure <- Cmd.start [bash|printf preserved|]",
    )
    .await;
    let backend = TestCommands::new();
    backend
        .output_unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    backend.finish.send_replace(true);
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let outcome = super::tests::dispatch_haskell_script_result(
        campaign.root_installation.policy.as_ref(),
        include_str!("command_binding_failure.hs"),
    )
    .await;
    let rendered = match outcome {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    };
    assert!(rendered.contains("automatic binding failed"), "{rendered}");
    assert!(!rendered.contains("Available binding:"), "{rendered}");
    assert!(!rendered.contains("Continue with:"), "{rendered}");
    let status = committed(&campaign, "Cmd.status retainedBeforeFailure").await;
    assert!(status.to_string().contains("CommandExited 0"), "{status}");
    assert_eq!(backend.specs.lock().len(), 1);
    assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn completed_command_output_survives_a_later_failure_in_the_same_computation() {
    let mut campaign = TestCampaign::start().await;
    committed(&campaign, "job <- Cmd.start [bash|printf result|]").await;
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let result = super::tests::dispatch_haskell_script_result(
        campaign.root_installation.policy.as_ref(),
        include_str!("command_prefix_failure.hs"),
    )
    .await;
    let rendered = match result {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    };
    assert!(rendered.contains("failure-after-command"), "{rendered}");
    assert!(
        rendered.contains("stdout ·"),
        "lost committed command output: {rendered}"
    );
    assert!(rendered.contains("result"), "{rendered}");
    assert_eq!(backend.specs.lock().len(), 1);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
