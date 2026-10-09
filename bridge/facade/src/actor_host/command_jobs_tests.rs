use super::command_test_support::TestCommands;
use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_lookup};
use super::*;
use exomonad_actor::command_jobs::{CommandBackendPurpose, CommandControl};
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
use tidepool_bridge_effects::*;

pub(super) async fn committed(campaign: &TestCampaign, source: &str) -> serde_json::Value {
    let result = dispatch_haskell_script(campaign.root_installation.policy.as_ref(), source).await;
    require_committed(&result);
    result
}

fn require_committed(result: &serde_json::Value) {
    assert_eq!(result["status"], "committed", "{result}");
    for item in result["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{result}");
    }
}
pub(super) use super::command_test_support::{backend_request, raw_backend_request};

#[test]
fn cargo_report_preserves_nonzero_diagnostics_and_typed_parse_errors() {
    use std::collections::VecDeque;
    use tidepool_effect::{EffectContext, EffectError, EffectHandler, Response};
    use tidepool_handlers::{
        ConsoleHandler, ExecError, ExecReq, FsReadHandler, FsWriteHandler, HttpHandler, KvHandler,
        Proc,
    };
    use tidepool_testing::eval_harness::EvalHarness;

    struct MockCargoExec {
        responses: VecDeque<Result<Proc, ExecError>>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl EffectHandler<tidepool_mcp::CapturedOutput> for MockCargoExec {
        type Request = ExecReq;

        fn handle(
            &mut self,
            request: Self::Request,
            context: &EffectContext<'_, tidepool_mcp::CapturedOutput>,
        ) -> Result<Response, EffectError> {
            assert!(
                matches!(request, ExecReq::RunArgv(_)),
                "Cargo should use runArgv"
            );
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            context.respond(
                self.responses
                    .pop_front()
                    .expect("one mock result per Cargo call"),
            )
        }
    }

    tidepool_testing::eval_harness::require_extract();
    let scratch = tempfile::tempdir().unwrap();
    std::fs::write(
        scratch.path().join("CargoReportContract.hs"),
        tidepool_testing::fixture_source("bridge/facade/src/actor_host/cargo_report_contract.hs"),
    )
    .unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handlers = frunk::hlist![
        ConsoleHandler,
        KvHandler::new(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(scratch.path()).unwrap(),
            "cargo-test-kv.json"
        )
        .unwrap(),
        FsReadHandler::new(scratch.path().to_path_buf()),
        FsWriteHandler::new(scratch.path().to_path_buf()),
        HttpHandler,
        MockCargoExec {
            responses: VecDeque::from([
                Ok(Proc {
                    exit_code: 101,
                    stdout: "{\"reason\":\"compiler-message\"}\n".into(),
                    stderr: "compiler failed".into(),
                }),
                Ok(Proc {
                    exit_code: 0,
                    stdout: "{}\n\nnot-json\n".into(),
                    stderr: String::new(),
                }),
                Ok(Proc {
                    exit_code: 101,
                    stdout: "{}\n\nnot-json\n".into(),
                    stderr: String::new(),
                }),
                Err(ExecError::ExecBadDir("unavailable".into())),
            ]),
            calls: Arc::clone(&calls),
        },
    ];
    let preamble = concat!(
        "{-# LANGUAGE DataKinds, FlexibleContexts, NoImplicitPrelude, TypeOperators #-}\n",
        "module Expr where\n",
        "import Tidepool.Prelude\n",
        "import Tidepool.Effects\n",
        "import Control.Monad.Freer (Eff)\n",
        "import qualified CargoReportContract\n",
    );
    let effect_stack = tidepool_mcp::build_effect_stack_type(&tidepool_mcp::standard_decls());
    let source = tidepool_runtime::session::assemble_expression_module(
        preamble,
        "result",
        &effect_stack,
        "CargoReportContract.result",
        tidepool_runtime::session::ExpressionLift::Effectful,
    );
    let harness = EvalHarness::new()
        .with_stdlib()
        .with_effects_module()
        .with_include(scratch.path().to_path_buf());
    let outcome = harness.run_with(
        &source,
        "result",
        handlers,
        tidepool_mcp::CapturedOutput::new(),
    );
    assert!(outcome.is_ok(), "Cargo fixture failed: {:?}", outcome.err());
    assert_eq!(outcome.json(), serde_json::json!([true, true, true, true]));
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 4);
}

#[tokio::test]
async fn ordinary_command_report_skips_unrequested_source_probe() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let running = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            "job <- Cmd.background [bash|printf ordinary|]\nreport <- await (Cmd.awaitFinished job)\nfmap Cmd.reportSource report",
        )
        .await
    });
    let command = raw_backend_request(campaign).await;
    assert_eq!(command.purpose, CommandBackendPurpose::Command);
    command.supply(Ok(TestCommands::completed("ordinary")));
    let result = running.await.unwrap();
    assert_eq!(result["status"], "committed", "{result}");
    assert!(result.to_string().contains("Nothing"), "{result}");
})).await;
}

#[tokio::test]
async fn structured_bash_uses_compiled_handler_and_shared_command_owner() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    assert!(policy.tools().iter().any(|tool| matches!(tool,
        exomonad_tool::HostedTool::Function(declaration) if declaration.name == "bash")));
    let script = "cat <<'EOF'\nλ; $(literal) [bash|text|]\nEOF\n";
    let invocation = ToolInvocation {
        name: "bash".into(),
        arguments: ToolArguments::Structured(serde_json::json!({"cmd":script})),
        context: Some(ToolInvocationContext::external(
            "raw-thread".into(),
            "raw-turn".into(),
            "raw-once".into(),
            Some("raw-once".into()),
            None,
        )),
    };
    let first = tokio::spawn(policy.dispatch_json_boxed(invocation.clone()));
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));
    let receipt = first.await.unwrap().unwrap();
    assert_eq!(receipt["status"], "committed", "{receipt}");
    let output = receipt["items"][0]["output"].as_str().unwrap();
    assert!(output.contains("result"), "{receipt}");
    // Every direct command tool call retains a Haskell binding, named in the
    // result text, even when output is small and nothing was truncated.
    let binding = receipt["items"][0]["installedBindings"][0]
        .as_str()
        .expect("a small structured command still installs a retained job binding");
    assert!(
        output.contains(&format!("retained as {binding} :: Cmd.Job")),
        "{receipt}"
    );
    assert_eq!(
        policy
            .dispatch_json_boxed(invocation.clone())
            .await
            .unwrap(),
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
    assert_eq!(backend.specs.lock()[0].memory, 1024 * 1024 * 1024);
    let mut changed = invocation.clone();
    changed.arguments = ToolArguments::Structured(serde_json::json!({"cmd":"changed"}));
    assert!(policy.dispatch_json_boxed(changed).await.is_err());

    let second_invocation = ToolInvocation {
        name: "bash".into(),
        arguments: ToolArguments::Structured(serde_json::json!({"cmd":"printf second-result"})),
        context: Some(ToolInvocationContext::external(
            "raw-thread".into(),
            "raw-next-turn".into(),
            "raw-second".into(),
            Some("raw-second".into()),
            None,
        )),
    };
    let before_second = tidepool_extract_cmd::extract_spawn_count();
    let second = tokio::spawn(policy.dispatch_json_boxed(second_invocation.clone()));
    let second_backend = TestCommands::completed("second-result");
    backend_request(campaign)
        .await
        .supply(Ok(second_backend.clone()));
    let second_receipt = second.await.unwrap().unwrap();
    let second_compiles = tidepool_extract_cmd::extract_spawn_count() - before_second;
    assert!(
        second_compiles <= 1,
        "a distinct job may issue its fresh binding interface, but must reuse the checked Job type; submitted {second_compiles} compiler requests"
    );
    assert_eq!(second_receipt["status"], "committed", "{second_receipt}");
    let second_binding = second_receipt["items"][0]["installedBindings"][0]
        .as_str()
        .expect("the second job has its own retained binding");
    assert_ne!(binding, second_binding);

    let before_replays = tidepool_extract_cmd::extract_spawn_count();
    for (call, original) in [(invocation, &receipt), (second_invocation, &second_receipt)] {
        assert_eq!(
            &policy.dispatch_json_boxed(call).await.unwrap(),
            original,
            "replay retains the original receipt even after another job was bound"
        );
    }
    assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before_replays);
    let recovered = committed(
        campaign,
        &format!(
            "retainedFirst <- Cmd.readStdout {binding}\nretainedSecond <- Cmd.readStdout {second_binding}\ndisplay (retainedFirst == Right \"result\" && retainedSecond == Right \"second-result\" && {binding} /= {second_binding})"
        ),
    )
    .await;
    assert_eq!(
        super::test_campaign::committed_display_text(&recovered),
        "True",
        "each distinct binding must keep its own job and retained output"
    );
    assert_eq!(backend.executions(), 1);
    assert_eq!(second_backend.executions(), 1);
})).await;
}

#[tokio::test]
async fn structured_shell_tools_retain_sessions_and_navigate_without_reexecution() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let call = |name: &str, arguments| {
        policy.dispatch_json_boxed(ToolInvocation {
            context: None,
            name: name.into(),
            arguments: ToolArguments::Structured(arguments),
        })
    };
    for (arguments, reason) in [
        (
            serde_json::json!({"cmd":"never", "yield_time_ms":-1}),
            "yield_time_ms must be 0..300000",
        ),
        (
            serde_json::json!({"cmd":"never", "yield_time_ms":300001}),
            "yield_time_ms must be 0..300000",
        ),
        (
            serde_json::json!({"cmd":"never", "max_output_bytes":0}),
            "max_output_bytes must be positive",
        ),
        (
            serde_json::json!({"cmd":"never", "memory_mib":0}),
            "memory_mib must be positive and fit Int bytes",
        ),
        (
            serde_json::json!({"cmd":"never", "memory_mib":-1}),
            "memory_mib must be positive and fit Int bytes",
        ),
        (
            serde_json::json!({"cmd":"never", "memory_mib":(i64::MAX / (1024 * 1024)) + 1}),
            "memory_mib must be positive and fit Int bytes",
        ),
        (
            serde_json::json!({"cmd":"never", "tty":true, "stdin":true}),
            "tty and stdin cannot both be true",
        ),
    ] {
        let invalid = call("bash", arguments).await.unwrap();
        assert_eq!(invalid["status"], "committed", "{invalid}");
        assert!(
            invalid["items"][0]["output"]
                .as_str()
                .unwrap()
                .contains(&format!("nothing started or sent · {reason}")),
            "{invalid}"
        );
    }
    let running = tokio::spawn(call(
        "bash",
        serde_json::json!({
            "cmd":"printf literal", "workdir":"src",
            "environment":[
                {"name":"EXAMPLE","value":"first"},
                {"name":"EXAMPLE","value":"value"}
            ],
            "memory_mib":64, "stdin":true, "yield_time_ms":0, "max_output_bytes":2048,
        }),
    ));
    let backend = TestCommands::new();
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));
    let receipt = running.await.unwrap().unwrap();
    assert_eq!(receipt["status"], "committed", "{receipt}");
    let text = receipt["items"][0]["output"].as_str().unwrap();
    let session = text
        .split("session_id: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let session = session.to_owned();
    let initial_reads = backend
        .slice_reads
        .load(std::sync::atomic::Ordering::Acquire);
    let initial_observations = backend.output_budgets().len();
    for (name, arguments, reason) in [
        (
            "cancel_command",
            serde_json::json!({"session_id":session,"yield_time_ms":300001}),
            "yield_time_ms must be 0..300000",
        ),
        (
            "cancel_command",
            serde_json::json!({"session_id":session,"max_output_bytes":0}),
            "max_output_bytes must be positive",
        ),
        (
            "read_output",
            serde_json::json!({"session_id":session,"offset":-1}),
            "offset must be nonnegative",
        ),
        (
            "read_output",
            serde_json::json!({"session_id":session,"max_output_bytes":0}),
            "max_output_bytes must be positive",
        ),
    ] {
        let invalid = call(name, arguments).await.unwrap();
        assert_eq!(invalid["status"], "committed", "{invalid}");
        assert!(
            invalid["items"][0]["output"]
                .as_str()
                .unwrap()
                .contains(&format!("nothing started or sent · {reason}")),
            "{invalid}"
        );
    }
    assert_eq!(
        backend.control_count(),
        0,
        "rejected calls sent input or cancellation"
    );
    assert_eq!(
        backend
            .slice_reads
            .load(std::sync::atomic::Ordering::Acquire),
        initial_reads,
        "rejected calls read retained output"
    );
    assert_eq!(
        backend.output_budgets().len(),
        initial_observations,
        "rejected calls observed output"
    );
    let binding = receipt["items"][0]["installedBindings"][0]
        .as_str()
        .expect("bash names a retained binding even with small output")
        .to_owned();
    let first = call(
        "write_stdin",
        serde_json::json!({"session_id":session,"yield_time_ms":0}),
    )
    .await
    .unwrap();
    assert_eq!(first["status"], "committed", "{first}");
    let repeated = call(
        "write_stdin",
        serde_json::json!({"session_id":session,"chars":"","yield_time_ms":0}),
    )
    .await
    .unwrap();
    assert!(
        !repeated["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("\nresult"),
        "{repeated}"
    );
    let read = call("read_output", serde_json::json!({"session_id":session}))
        .await
        .unwrap();
    let read_output = read["items"][0]["output"].as_str().unwrap();
    assert!(read_output.contains("result"), "{read}");
    // Each hosted call may name its retained handle in a fresh local binding;
    // the session identity and returned bytes establish that this is the same job.
    let read_binding = read["items"][0]["installedBindings"][0].as_str().unwrap();
    assert!(
        read_output.contains(&format!("session_id: {session}")),
        "{read}"
    );
    assert!(
        read_output.contains(&format!("retained as {read_binding} :: Cmd.Job")),
        "{read}"
    );
    let observations_before_write = backend.output_budgets().len();
    let controls_before_write = backend.control_count();
    let input = call(
        "write_stdin",
        serde_json::json!({
            "session_id":session,"chars":"hello\n","yield_time_ms":-1,
            "max_output_bytes":0
        }),
    )
    .await
    .unwrap();
    assert_eq!(input["status"], "committed", "{input}");
    assert!(
        input["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("Input acknowledged by backend"),
        "{input}"
    );
    assert_eq!(
        backend.control_count(),
        controls_before_write + 1,
        "the invalid observation fields must not prevent the input write"
    );
    assert_eq!(
        backend.output_budgets().len(),
        observations_before_write,
        "a committed input receipt does not also observe or present output"
    );
    backend.finish.send_replace(true);
    let finished = call(
        "write_stdin",
        serde_json::json!({"session_id":session,"yield_time_ms":1000}),
    )
    .await
    .unwrap();
    assert!(
        finished["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("CommandExited 0"),
        "{finished}"
    );
    // The binding named across four separate direct tool calls resolves in a
    // later cell exactly like a cell-created binding — no rerun, no refetch.
    let resolved = committed(campaign, &format!("Cmd.status {binding}")).await;
    assert!(
        resolved.to_string().contains("CommandExited 0"),
        "{resolved}"
    );
    let foreign = call(
        "write_stdin",
        serde_json::json!({"session_id":"unowned","yield_time_ms":0}),
    )
    .await;
    assert!(foreign.is_err(), "{foreign:?}");
    assert_eq!(
        *backend.specs.lock(),
        vec![CommandSpec {
            argv: vec![
                "bash".into(),
                "--noprofile".into(),
                "--norc".into(),
                "-c".into(),
                "printf literal".into(),
                "exomonad-bash".into()
            ],
            directory: Some("src".into()),
            environment: vec![("EXAMPLE".into(), "value".into())],
            memory: 64 * 1024 * 1024,
            input: CommandInput::PipeInput,
            source_capture: CommandSourceCapture::NoCapture,
        }]
    );
})).await;
}

#[tokio::test]
async fn structured_bash_oversized_output_retains_a_real_job() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let backend = TestCommands::new();
                *backend.stdout.lock() = format!("BEGIN\n{}\nEND\n", "λ".repeat(32_000));
                backend.finish.send_replace(true);
                let policy = campaign.root_installation.policy.clone();
                let running = tokio::spawn(policy.dispatch_json_boxed(ToolInvocation {
                    context: None,
                    name: "bash".into(),
                    arguments: ToolArguments::Structured(serde_json::json!({"cmd":"large"})),
                }));
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let response = running.await.unwrap().unwrap();
                assert_eq!(response["status"], "committed", "{response}");
                let output = response["items"][0]["output"].as_str().unwrap();
                assert!(output.len() < 10 * 1024, "{}", output.len());
                assert!(
                    output.contains("BEGIN") && output.contains("END"),
                    "{output}"
                );
                let session = output
                    .split("session_id: ")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap();
                let page = policy
                    .dispatch_json_boxed(ToolInvocation {
                        context: None,
                        name: "read_output".into(),
                        arguments: ToolArguments::Structured(
                            serde_json::json!({"session_id":session}),
                        ),
                    })
                    .await
                    .unwrap();
                let page_text = page["items"][0]["output"].as_str().unwrap();
                assert!(
                    page_text.contains("BEGIN") && page_text.contains("next_offset:"),
                    "{page_text}"
                );
                assert!(page_text.len() <= 8192);
                assert!(!page_text.contains("END"));
                let offset: i64 = page_text
                    .split("next_offset: ")
                    .nth(1)
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                let next_page = policy
        .dispatch_json_boxed(ToolInvocation {
            context: None,
            name: "read_output".into(),
            arguments: ToolArguments::Structured(
                serde_json::json!({"session_id":session,"offset":offset,"max_output_bytes":1024}),
            ),
        })
        .await
        .unwrap();
                assert!(
                    next_page["items"][0]["output"]
                        .as_str()
                        .unwrap()
                        .contains(&format!("bytes {offset}–")),
                    "{next_page}"
                );
                assert!(next_page["items"][0]["output"].as_str().unwrap().len() <= 1024);
                let binding = response["items"][0]["installedBindings"][0]
                    .as_str()
                    .unwrap();
                assert!(
                    output.contains(&format!("{binding} :: Cmd.Job")),
                    "{output}"
                );
                let observed = committed(
                    campaign,
                    &format!("saved <- Cmd.readStdout {binding}\nfmap T.length saved"),
                )
                .await;
                assert!(observed.to_string().contains("32011"), "{observed}");
                assert_eq!(backend.specs.lock().len(), 1);
            })
        })
        .await;
}

#[tokio::test]
async fn structured_bash_timeout_preserves_the_command_for_haskell_continuation() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let backend = TestCommands::new();
                let policy = campaign.root_installation.policy.clone();
                let running = tokio::spawn(policy.dispatch_json_boxed(ToolInvocation {
                    context: None,
                    name: "bash".into(),
                    arguments: ToolArguments::Structured(
                        serde_json::json!({"cmd":"long-lived", "yield_time_ms":0}),
                    ),
                }));
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let response = running.await.unwrap().unwrap();
                assert_eq!(response["status"], "committed", "{response}");
                let output = response["items"][0]["output"].as_str().unwrap();
                let session = output
                    .split("session_id: ")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap();
                // The notice for a backgrounded command carries both the attempt's
                // identity (session_id) and the retained handle (Haskell binding)
                // together, so a model can act on it without re-deriving either. Nothing
                // here supports calling the job superseded — the notice must not guess
                // that either.
                let notice_binding = response["items"][0]["installedBindings"][0]
                    .as_str()
                    .unwrap();
                assert!(
                    output.contains(&format!("session_id: {session}")),
                    "{output}"
                );
                assert!(
                    output.contains(&format!("{notice_binding} :: Cmd.Job")),
                    "{output}"
                );
                assert!(!output.contains("superseded"), "{output}");
                let direct = policy
                    .dispatch_json_boxed(ToolInvocation {
                        context: None,
                        name: "write_stdin".into(),
                        arguments: ToolArguments::Structured(
                            serde_json::json!({"session_id":session,"yield_time_ms":0}),
                        ),
                    })
                    .await
                    .unwrap();
                assert_eq!(direct["status"], "committed", "{direct}");
                let binding = response["items"][0]["installedBindings"][0]
                    .as_str()
                    .unwrap();
                assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
                backend.finish.send_replace(true);
                let observed = committed(
                    campaign,
                    &format!("saved <- Cmd.await {binding}\nCmd.stdout saved"),
                )
                .await;
                assert!(observed.to_string().contains("result"), "{observed}");
                assert_eq!(backend.specs.lock().len(), 1);
            })
        })
        .await;
}

/// A launch introduces the retained job once. Later observations and
/// cancellation use the supplied session without repeating the introduction.

#[tokio::test]
async fn later_command_tools_keep_recovery_without_repeating_binding_introductions() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let backend = TestCommands::new();
                let policy = campaign.root_installation.policy.clone();
                let call = |name: &str, arguments| {
                    policy.dispatch_json_boxed(ToolInvocation {
                        context: None,
                        name: name.into(),
                        arguments: ToolArguments::Structured(arguments),
                    })
                };
                let running = tokio::spawn(call(
                    "bash",
                    serde_json::json!({"cmd":"printf literal", "stdin":true, "yield_time_ms":0}),
                ));
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let started = running.await.unwrap().unwrap();
                let started_binding = started["items"][0]["installedBindings"][0]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let session = started["items"][0]["output"]
                    .as_str()
                    .unwrap()
                    .split("session_id: ")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .to_owned();

                let write = call(
                    "write_stdin",
                    serde_json::json!({"session_id":session,"chars":"","yield_time_ms":0}),
                )
                .await
                .unwrap();
                assert_eq!(write["status"], "committed", "{write}");
                let write_output = write["items"][0]["output"].as_str().unwrap();
                assert!(!write_output.contains("retained as"), "{write}");
                assert!(!write_output.contains("session_id:"), "{write}");

                let cancel = call(
                    "cancel_command",
                    serde_json::json!({"session_id":session,"yield_time_ms":0}),
                )
                .await
                .unwrap();
                assert_eq!(cancel["status"], "committed", "{cancel}");
                let cancel_output = cancel["items"][0]["output"].as_str().unwrap();
                assert!(!cancel_output.contains("retained as"), "{cancel}");
                assert!(!cancel_output.contains("session_id:"), "{cancel}");
                // The original launch binding remains usable after later tool calls.

                let resolved = committed(
        campaign,
        &format!("saved <- Cmd.await {started_binding}\ndisplay =<< Cmd.status {started_binding}"),
    )
    .await;
                assert!(resolved.to_string().contains("Cancelled"), "{resolved}");
            })
        })
        .await;
}

#[tokio::test]
async fn command_jobs_retain_completion_and_route_to_record_actors() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let initial = committed(
        campaign,
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/command_jobs.hs"),
    )
    .await;
    assert!(initial.to_string().contains("CommandQueued"), "{initial}");
    let backend = TestCommands::new();
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));
    backend.finish.send_replace(true);
    let result = committed(
        campaign,
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
            "exomonad-bash",
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
        campaign,
        "late <- R.start collector\nR.call (completionCount (R.client late)) ()",
    )
    .await;
    assert_eq!(late["items"][1]["output"], "1", "{late}");
    let early = committed(
        campaign,
        "R.call (completionCount (R.client listener)) ()\nR.finish listener\nR.finish late",
    )
    .await;
    assert_eq!(early["items"][0]["output"], "1", "{early}");
})).await;
}

#[tokio::test]
async fn inherited_command_is_readable_without_transferring_control_or_display_position() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    committed(
        campaign,
        "job <- do { issued <- Cmd.start (Cmd.withStdin [bash|printf inherited|]); Cmd.detach issued; pure issued }",
    )
    .await;
    let backend = TestCommands::new();
    *backend.stdout.lock() = "first-line\n".into();
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));

    let launch = {
        let root = campaign.root_installation.policy.clone();
        tokio::spawn(async move {
            dispatch_haskell_script(
                root.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/inherited_command_observer.hs",
                ),
            )
            .await
        })
    };
    let child = campaign
        .next_deployment(
            "inherited command observer",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        child.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
            integrate: true,
        },
    );
    let _custody = child
        .worktree_custody
        .clone()
        .expect("child checkout binding remains live for this test");
    campaign.acknowledge_native_spawn(&child);
    require_committed(&launch.await.unwrap());
    campaign
        .next_deployment(
            "inherited command observer ready",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let observer = child.policy.as_ref();

    // The inherited job is the same handle, but presentation state belongs to
    // each caller. The first observer cannot consume the owner's first page.
    let observed =
        dispatch_haskell_script(observer, "Cmd.observe (Cmd.Observation 1000 65536) job").await;
    assert_eq!(observed["status"], "committed", "{observed}");
    assert!(observed.to_string().contains("first-line"), "{observed}");
    let repeated =
        dispatch_haskell_script(observer, "Cmd.observe (Cmd.Observation 0 65536) job").await;
    assert_eq!(repeated["status"], "committed", "{repeated}");
    assert!(!repeated.to_string().contains("first-line"), "{repeated}");
    let owner_first = committed(campaign, "Cmd.observe (Cmd.Observation 0 65536) job").await;
    assert!(
        owner_first.to_string().contains("first-line"),
        "{owner_first}"
    );

    *backend.stdout.lock() = "first-line\nsecond-line\n".into();
    let owner_next = committed(campaign, "Cmd.observe (Cmd.Observation 0 65536) job").await;
    assert!(
        owner_next.to_string().contains("second-line"),
        "{owner_next}"
    );
    assert!(
        !owner_next.to_string().contains("first-line"),
        "{owner_next}"
    );
    let observer_next =
        dispatch_haskell_script(observer, "Cmd.observe (Cmd.Observation 0 65536) job").await;
    assert_eq!(observer_next["status"], "committed", "{observer_next}");
    assert!(
        observer_next.to_string().contains("second-line"),
        "{observer_next}"
    );
    assert!(
        !observer_next.to_string().contains("first-line"),
        "{observer_next}"
    );

    let status = dispatch_haskell_script(observer, "Cmd.status job").await;
    assert_eq!(status["status"], "committed", "{status}");
    let output = dispatch_haskell_script(observer, "Cmd.pageText <$> Cmd.output job").await;
    assert_eq!(output["status"], "committed", "{output}");
    assert!(output.to_string().contains("first-line"), "{output}");
    assert!(output.to_string().contains("second-line"), "{output}");

    for (operation, refusal) in [
        ("Cmd.sendInput job \"foreign\"", "not authorized"),
        ("Cmd.closeInput job", "not authorized"),
        ("Cmd.resize job 40 80", "not authorized"),
        ("Cmd.cancel job", "not authorized"),
        ("Cmd.detach job", "not authorized"),
        ("Cmd.start (Cmd.argv [])", "not runnable"),
        ("Cmd.background (Cmd.argv [])", "not runnable"),
    ] {
        let discarded = format!("do {{ {operation}; pure (424242 :: Int) }}");
        let denial = super::tests::dispatch_haskell_script_result(observer, &discarded).await;
        let rendered = match denial {
            Ok(value) => {
                assert_ne!(
                    value["status"], "committed",
                    "discarded command refusal must stop its continuation: {value}"
                );
                value.to_string()
            }
            Err(error) => error.to_string(),
        };
        assert!(rendered.contains(refusal), "{operation}: {rendered}");
    }
    assert_eq!(
        backend.control_count(),
        0,
        "foreign control reached backend"
    );
    committed(campaign, "Cmd.sendInput job \"owner\"").await;
    assert_eq!(backend.control_count(), 1, "owner lost command control");

    backend.finish();
    let completed = dispatch_haskell_script(observer, "Cmd.await job").await;
    assert_eq!(completed["status"], "committed", "{completed}");
    assert!(
        completed.to_string().contains("CommandExited 0"),
        "{completed}"
    );
    assert_eq!(backend.executions(), 1, "foreign reads reran the command");
})).await;
}

#[tokio::test]
async fn inherited_command_helpers_start_fresh_jobs_in_each_callers_checkout() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let fixed = campaign._repository.path().join("fixed-source");
    let fixed_text = fixed.to_string_lossy().into_owned();
    committed(
        campaign,
        &format!("let fixedPath = {:?} :: Text", fixed_text),
    )
    .await;
    committed(
        campaign,
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/inherited_command_helpers.hs",
        ),
    )
    .await;
    let launch = {
        let root = campaign.root_installation.policy.clone();
        tokio::spawn(async move {
            dispatch_haskell_script(
                root.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/inherited_command_observer.hs",
                ),
            )
            .await
        })
    };
    let child = campaign
        .next_deployment(
            "inherited helper observer",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        child.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
            integrate: true,
        },
    );
    campaign.acknowledge_native_spawn(&child);
    require_committed(&launch.await.unwrap());
    campaign
        .next_deployment(
            "inherited helper observer ready",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;

    let launches =
        "freshJob <- launchFresh ()\nCmd.detach freshJob\nrelativeJob <- launchRelative ()\nCmd.detach relativeJob\nfixedJob <- launchFixed ()\nCmd.detach fixedJob";
    let awaits = "Cmd.await freshJob\nCmd.await relativeJob\nCmd.await fixedJob";
    for (policy, owner) in [
        (
            campaign.root_installation.policy.clone(),
            campaign.actor.identity(),
        ),
        (child.policy.clone(), child.actor.identity()),
    ] {
        let launched = dispatch_haskell_script(policy.as_ref(), launches).await;
        assert_eq!(launched["status"], "committed", "{launched}");
        let backend = TestCommands::completed("cwd captured");
        for _ in 0..3 {
            let request = backend_request(campaign).await;
            assert_eq!(request.owner, owner);
            request.supply(Ok(backend.clone()));
        }
        let awaited = dispatch_haskell_script(policy.as_ref(), awaits).await;
        assert_eq!(awaited["status"], "committed", "{awaited}");
        let mut directories = backend
            .specs
            .lock()
            .iter()
            .map(|spec| spec.directory.clone())
            .collect::<Vec<_>>();
        directories.sort();
        let mut expected = vec![None, Some("subdir".into()), Some(fixed_text.clone())];
        expected.sort();
        assert_eq!(directories, expected);
    }
    let root_checkout = resident_command_roots(
        &campaign.authority,
        &campaign.worktrees,
        campaign._repository.path(),
        campaign.actor.identity(),
    )
    .unwrap();
    let child_checkout = resident_command_roots(
        &campaign.authority,
        &campaign.worktrees,
        campaign._repository.path(),
        child.actor.identity(),
    )
    .unwrap();
    assert_eq!(root_checkout.directory, campaign._repository.path());
    assert_ne!(child_checkout.directory, root_checkout.directory);
    assert!(child_checkout.custody);
})).await;
}

#[tokio::test]
async fn extracted_effectful_closure_starts_work_in_receiver_after_response_release() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let launch = {
        let root = campaign.root_installation.policy.clone();
        tokio::spawn(async move {
            dispatch_haskell_script(
                root.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/inherited_effectful_producer.hs",
                ),
            )
            .await
        })
    };
    let producer = campaign
        .next_deployment(
            "effectful producer",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        producer.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
            integrate: true,
        },
    );
    campaign.acknowledge_native_spawn(&producer);
    require_committed(&launch.await.unwrap());
    campaign
        .next_deployment(
            "effectful producer ready",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;

    let launch = {
        let root = campaign.root_installation.policy.clone();
        tokio::spawn(async move {
            dispatch_haskell_script(
                root.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/inherited_effectful_observer.hs",
                ),
            )
            .await
        })
    };
    let observer = campaign
        .next_deployment(
            "effectful observer",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        observer.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
            integrate: true,
        },
    );
    campaign.acknowledge_native_spawn(&observer);
    require_committed(&launch.await.unwrap());
    campaign
        .next_deployment(
            "effectful observer ready",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;

    let replied = dispatch_haskell_script(
        producer.policy.as_ref(),
        "respond ((\\() -> Cmd.start (Cmd.argv [\"pwd\"])) :: () -> Eff '[Replies, Commands, Lookup, BoundWorktree] Cmd.Job)",
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    let extracted = dispatch_haskell_script(
        observer.policy.as_ref(),
        "observed <- pollResponse worker\nlet freshClosure = case observed of { ResponseReady answer -> responseValue answer; _ -> error \"effectful response was not ready\" }",
    )
    .await;
    assert_eq!(extracted["status"], "committed", "{extracted}");
    let released = committed(campaign, "forgetResponse worker").await;
    assert!(
        released.to_string().contains("ResponseForgotten"),
        "{released}"
    );

    let started = dispatch_haskell_script(
        observer.policy.as_ref(),
        "createdJob <- freshClosure ()\nCmd.detach createdJob",
    )
    .await;
    assert_eq!(started["status"], "committed", "{started}");
    let backend = TestCommands::completed("receiver checkout");
    let request = backend_request(campaign).await;
    assert_eq!(request.owner, observer.actor.identity());
    request.supply(Ok(backend.clone()));
    let completed = dispatch_haskell_script(observer.policy.as_ref(), "Cmd.await createdJob").await;
    assert_eq!(completed["status"], "committed", "{completed}");
    assert_eq!(backend.executions(), 1);
    {
        let specs = backend.specs.lock();
        assert_eq!(specs[0].argv, ["pwd"]);
        assert!(specs[0].directory.is_none());
    }
})).await;
}

#[tokio::test]
async fn command_output_ux_preserves_large_values_and_decodes_complete_stdout() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                committed(
                    campaign,
                    "job <- Cmd.start [bash|printf result|]\nCmd.detach job",
                )
                .await;
                let backend = TestCommands::new();
                backend.finish.send_replace(true);
                backend_request(campaign).await.supply(Ok(backend));
                committed(campaign, "finished <- Cmd.await job").await;
                let result = committed(
                    campaign,
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/command_output_ux.hs",
                    ),
                )
                .await;
                let text = result.to_string();
                for marker in [
                    "large-display-ok",
                    "json-ok",
                    "partial-rejected",
                    "streams-independent",
                    "decode-error-distinct",
                    "omission-kinds-preserved",
                    "unavailable-output-typed",
                ] {
                    assert!(text.contains(marker), "missing {marker}: {text}");
                }
                let info = dispatch_lookup(
                    campaign.root_installation.policy.as_ref(),
                    &["Cmd.RunResult"],
                )
                .await;
                assert!(info.to_string().contains("data RunResult"), "{info}");
                assert!(!text.contains("Display failed"), "{text}");
            })
        })
        .await;
}

/// A failed command's diagnostic output is exactly what a caller wants, and is
/// what `Cmd.stdout`/`Cmd.readStdout` deliberately refuse. `Cmd.readCommand`
/// serves it — without letting an incomplete capture read as a complete string.
#[tokio::test]
async fn read_command_captures_both_streams_of_a_failed_command() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                committed(campaign, "job <- Cmd.start [bash|exit 3|]\nCmd.detach job").await;
                let backend = TestCommands::new();
                *backend.stdout.lock() = "standard out".into();
                *backend.stderr.lock() = "boom: file not found".into();
                backend
                    .exit_code
                    .store(3, std::sync::atomic::Ordering::Release);
                backend.finish.send_replace(true);
                backend_request(campaign).await.supply(Ok(backend.clone()));
                committed(campaign, "finished <- Cmd.await job").await;

                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
                let policy = campaign.root_installation.policy.clone();
                let whole = campaign
                    .drive_actor_output(
                        &store,
                        super::test_campaign::dispatch_haskell_script(
                            policy.as_ref(),
                            &tidepool_testing::fixture_source(
                                "bridge/facade/src/actor_host/command_capture.hs",
                            ),
                        ),
                    )
                    .await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&whole),
                    "True",
                    "{whole}"
                );

                // The same retained job, now serving pages that rotated bytes away, decoded
                // lossily, and stop short of end of file.
                backend
                    .degraded_output
                    .store(true, std::sync::atomic::Ordering::Release);
                let degraded = campaign
                    .drive_actor_output(
                        &store,
                        super::test_campaign::dispatch_haskell_script(
                            policy.as_ref(),
                            &tidepool_testing::fixture_source(
                                "bridge/facade/src/actor_host/command_capture_lossy.hs",
                            ),
                        ),
                    )
                    .await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&degraded),
                    "True",
                    "{degraded}"
                );
                assert_eq!(backend.executions(), 1, "reading must not run the command");
            })
        })
        .await;
}

#[tokio::test]
async fn bound_command_result_is_summarized_and_remains_readable() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let launched = tokio::spawn(async move {
                    dispatch_haskell_script(policy.as_ref(), "paged <- Cmd.run [bash|printf Q|]")
                        .await
                });
                let backend = TestCommands::completed(&"Q".repeat(10000));
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let run = launched.await.unwrap();
                assert_eq!(run["status"], "committed", "{run}");
                assert_eq!(backend.specs.lock().len(), 1);

                let bound_output = run["items"][0]["output"].as_str().unwrap();
                assert!(bound_output.contains("command "), "{run}");
                assert!(bound_output.contains("exit 0"), "{run}");
                assert!(bound_output.contains("stdout 10000 bytes"), "{run}");
                assert!(bound_output.contains("stderr 0 bytes"), "{run}");
                assert!(!bound_output.contains(&"Q".repeat(100)), "{run}");
                let first = committed(campaign, "paged").await;
                assert!(
                    first["items"][0]["output"]
                        .as_str()
                        .unwrap()
                        .contains("stdout"),
                    "{first}"
                );
                let recovered = committed(
                    campaign,
                    "Cmd.stdout paged == Right (T.replicate 10000 \"Q\")",
                )
                .await;
                assert_eq!(recovered["items"][0]["output"], "True", "{recovered}");

                let policy = campaign.root_installation.policy.clone();
                let unbound_run = tokio::spawn(async move {
                    dispatch_haskell_script(policy.as_ref(), "Cmd.run [bash|printf U|]").await
                });
                let unbound_backend = TestCommands::completed("U");
                backend_request(campaign)
                    .await
                    .supply(Ok(unbound_backend.clone()));
                let unbound = unbound_run.await.unwrap();
                let unbound_output = unbound["items"][0]["output"].as_str().unwrap();
                assert!(unbound_output.contains("CommandExited 0"), "{unbound}");
                assert!(!unbound_output.contains("session_id:"), "{unbound}");
                assert!(unbound_output.contains('U'), "{unbound}");
                assert_eq!(backend.specs.lock().len(), 1, "reading reran the command");
                assert_eq!(unbound_backend.specs.lock().len(), 1);
            })
        })
        .await;
}

#[tokio::test]
async fn oom_command_result_names_the_applied_limit_and_a_rerun_hint() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let launched = tokio::spawn(async move {
                    dispatch_haskell_script(policy.as_ref(), "Cmd.run [bash|python3 -c oom|]").await
                });
                let backend = TestCommands::completed_oom(2048);
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let run = launched.await.unwrap();
                assert_eq!(run["status"], "committed", "{run}");

                let output = run["items"][0]["output"].as_str().unwrap();
                assert!(
                    output.contains(
                        "out of memory · memory_mib=2048 exceeded · rerun with a larger memory_mib"
                    ),
                    "{run}"
                );
                // A killed process does not read as a real exit code.
                assert!(!output.contains("CommandExited"), "{run}");
            })
        })
        .await;
}

#[tokio::test]
async fn cancelled_command_result_projects_and_later_cells_still_run() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                committed(
                    campaign,
                    "job <- Cmd.start [bash|sleep 30|]\nCmd.detach job",
                )
                .await;
                let backend = TestCommands::new();
                backend_request(campaign).await.supply(Ok(backend.clone()));
                committed(campaign, "Cmd.cancel job").await;
                committed(campaign, "cancelled <- Cmd.await job").await;
                let projected = committed(
                    campaign,
                    "(Cmd.commandResult cancelled, Cmd.stdout cancelled)",
                )
                .await;
                let text = projected.to_string();
                assert!(!text.contains("Display failed"), "{text}");
                assert!(text.contains("CommandCancelled"), "{text}");
                let later = committed(campaign, "1 + 1").await;
                assert_eq!(later["items"][0]["output"], "2", "{later}");
                assert_eq!(backend.specs.lock().len(), 1);
            })
        })
        .await;
}

#[tokio::test]
async fn failed_command_display_retains_result_without_reexecution() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                committed(
                    campaign,
                    "job <- Cmd.start [bash|printf result|]\nCmd.detach job",
                )
                .await;
                let backend = TestCommands::new();
                backend.finish.send_replace(true);
                backend_request(campaign).await.supply(Ok(backend.clone()));
                committed(campaign, "finished <- Cmd.await job").await;
                let failed = committed(
                    campaign,
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/command_display_failure.hs",
                    ),
                )
                .await;
                let text = failed.to_string();
                assert!(text.contains("Display failed"), "{text}");
                assert!(text.contains("Value remains bound"), "{text}");
                assert!(!text.contains("Expand: inspectFull"), "{text}");
                let recovered = committed(campaign, "Cmd.stdout (savedResult broken)").await;
                assert!(
                    recovered.to_string().contains("Right \\\"result\\\""),
                    "{recovered}"
                );
                assert_eq!(backend.specs.lock().len(), 1);
            })
        })
        .await;
}

#[tokio::test]
async fn command_jobs_cancel_before_backend_cannot_start_later() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let started = campaign
                    .root_installation
                    .policy
                    .dispatch_json_boxed(ToolInvocation {
                        context: None,
                        name: "bash".into(),
                        arguments: ToolArguments::Structured(
                            serde_json::json!({"cmd":"never-execute","background":true}),
                        ),
                    })
                    .await
                    .unwrap();
                let text = started["items"][0]["output"].as_str().unwrap();
                let session = text
                    .split("session_id: ")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap();
                for _ in 0..2 {
                    let cancelled = campaign
                        .root_installation
                        .policy
                        .dispatch_json_boxed(ToolInvocation {
                            context: None,
                            name: "cancel_command".into(),
                            arguments: ToolArguments::Structured(
                                serde_json::json!({"session_id":session,"yield_time_ms":1000}),
                            ),
                        })
                        .await
                        .unwrap();
                    assert!(
                        cancelled.to_string().contains("CommandCancelled"),
                        "{cancelled}"
                    );
                }
                let backend = TestCommands::completed(
                    "/work/tree\n0123456789abcdef0123456789abcdef01234567\nclean\n",
                );
                let request = raw_backend_request(campaign).await;
                assert_eq!(request.purpose, CommandBackendPurpose::SourceProbe);
                request.supply(Ok(backend.clone()));
                let result = campaign
                    .root_installation
                    .policy
                    .dispatch_json_boxed(ToolInvocation {
                        context: None,
                        name: "write_stdin".into(),
                        arguments: ToolArguments::Structured(
                            serde_json::json!({"session_id":session,"yield_time_ms":0}),
                        ),
                    })
                    .await
                    .unwrap();
                assert!(result.to_string().contains("CommandCancelled"), "{result}");
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), raw_backend_request(campaign))
                        .await
                        .is_err(),
                    "a cancelled job requested a command backend after its late source probe"
                );
                assert!(backend
                    .specs
                    .lock()
                    .iter()
                    .all(|spec| spec.argv[4].contains("git rev-parse")));
            })
        })
        .await;
}

/// The actor kernel admits one active turn at a time, and the `Control`
/// `Cancel` branch of `JobActor::handle` runs inside that turn. A backend
/// whose `control(.., Cancel)` never resolves must not freeze the actor
/// forever: the call is bounded, and cancel comes back with an error rather
/// than hanging.
#[tokio::test]
async fn command_cancel_is_bounded_when_the_backend_never_confirms() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let running = tokio::spawn(policy.clone().dispatch_json_boxed(ToolInvocation {
                    context: None,
                    name: "bash".into(),
                    arguments: ToolArguments::Structured(
                        serde_json::json!({"cmd":"long-lived", "yield_time_ms":0}),
                    ),
                }));
                let backend = TestCommands::new();
                backend.hang_cancel();
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let response = running.await.unwrap().unwrap();
                let output = response["items"][0]["output"].as_str().unwrap();
                let session = output
                    .split("session_id: ")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap();

                let started = std::time::Instant::now();
                let cancelled = policy
                    .dispatch_json_boxed(ToolInvocation {
                        context: None,
                        name: "cancel_command".into(),
                        arguments: ToolArguments::Structured(
                            serde_json::json!({"session_id":session,"yield_time_ms":1000}),
                        ),
                    })
                    .await
                    .unwrap();
                let elapsed = started.elapsed();
                assert!(
        elapsed < std::time::Duration::from_secs(8),
        "cancel with a stuck backend must return within the actor-turn bound, took {elapsed:?}"
    );
                assert!(
                    cancelled.to_string().contains("not confirmed within"),
                    "{cancelled}"
                );
                backend.finish.send_replace(true);
            })
        })
        .await;
}

#[tokio::test]
async fn command_hidden_by_the_response_budget_remains_unobserved() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
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
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let first = running.await.unwrap();
                assert_eq!(first["status"], "committed");
                assert!(!first.to_string().contains("stdout ·"));
                let next = committed(campaign, "Cmd.await (Cmd.job hidden)").await;
                assert!(next.to_string().contains("stdout ·"), "{next}");
                assert!(next.to_string().contains("result"), "{next}");
                assert_eq!(backend.specs.lock().len(), 1);
            })
        })
        .await;
}

#[tokio::test]
async fn disconnected_command_wait_retries_the_same_invocation_without_reexecution() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let backend = TestCommands::new();
                *backend.stdout.lock() = "first".into();
                let call_id = uuid::Uuid::new_v4().to_string();
                let invocation = || ToolInvocation {
                    context: Some(ToolInvocationContext::external(
                        "disconnected-command-wait".into(),
                        call_id.clone(),
                        call_id.clone(),
                        Some(call_id.clone()),
                        Some("haskell".into()),
                    )),
                    name: exomonad_actor::HASKELL_TOOL.into(),
                    arguments: ToolArguments::Raw(tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/command_wait_continuation.hs",
                    )),
                };
                let policy = campaign.root_installation.policy.clone();
                let first = invocation();
                let waiter = tokio::spawn(async move { policy.dispatch_json_boxed(first).await });
                backend_request(campaign).await.supply(Ok(backend.clone()));
                tokio::time::timeout(Duration::from_secs(30), async {
                    while backend.executions() == 0 {
                        assert!(!waiter.is_finished(), "command call ended before execution");
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("command was not started");
                assert!(
                    !waiter.is_finished(),
                    "terminal wait returned before completion"
                );
                waiter.abort();
                assert!(waiter.await.unwrap_err().is_cancelled());
                assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
                backend.finish();
                let second = TestCommands::completed("second");
                backend_request(campaign).await.supply(Ok(second.clone()));
                let suffix = TestCommands::completed("suffix");
                backend_request(campaign).await.supply(Ok(suffix.clone()));
                let policy = campaign.root_installation.policy.as_ref();
                let recovered = policy.dispatch_json_boxed(invocation()).await.unwrap();
                let repeated = policy.dispatch_json_boxed(invocation()).await.unwrap();
                assert_eq!(
                    recovered, repeated,
                    "exact transport retry changed the receipt"
                );
                assert_eq!(recovered["status"], "committed", "{recovered}");
                assert!(
                    recovered.to_string().contains("suffix-resumed"),
                    "{recovered}"
                );
                policy
                    .complete_boxed(
                        tidepool_runtime::session::ContextCheckpointBoundary::external(
                            "disconnected-command-wait".into(),
                            call_id.clone(),
                            call_id,
                        ),
                    )
                    .await
                    .unwrap();
                let retained = committed(campaign, "Cmd.stdout (fst attempt)").await;
                assert!(retained.to_string().contains("first"), "{retained}");
                for backend in [&backend, &second, &suffix] {
                    assert_eq!(
                        backend.executions(),
                        1,
                        "transport retry replayed a command"
                    );
                    assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
                }
            })
        })
        .await;
}

#[tokio::test]
async fn command_run_returns_typed_output_failure_and_resumes_the_suffix() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let running = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            &tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/command_wait_continuation.hs",
            ),
        )
        .await
    });
    let backend = TestCommands::completed("first");
    backend
        .output_unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));
    let second = TestCommands::completed("second");
    backend_request(campaign)
        .await
        .supply(Ok(second.clone()));
    let suffix = TestCommands::completed("suffix");
    backend_request(campaign)
        .await
        .supply(Ok(suffix.clone()));
    let result = running.await.unwrap();
    assert_eq!(result["status"], "committed", "{result}");
    for item in result["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{result}");
    }
    let projected = committed(campaign,
        "(Cmd.commandOutcome (Cmd.commandResult (fst attempt)), Cmd.capturedOutput (fst attempt), Cmd.stdout (fst attempt), Cmd.stderr (fst attempt))",
    ).await;
    let typed = projected["items"][0]["output"].as_str().unwrap();
    assert!(typed.contains("CommandExited 0"), "{typed}");
    assert!(typed.contains("CommandUnavailable"), "{typed}");
    assert_eq!(typed.matches("OutputUnavailable").count(), 2, "{typed}");
    let text = result.to_string();
    assert!(text.contains("output transport lost"), "{text}");
    for marker in ["second", "suffix", "suffix-resumed"] {
        assert!(text.contains(marker), "missing {marker}: {text}");
    }
    let prefix = committed(campaign, "foregroundPrefix").await;
    assert!(prefix.to_string().contains("prefix-preserved"), "{prefix}");
    backend
        .output_unavailable
        .store(false, std::sync::atomic::Ordering::Release);
    let recovered = committed(
        campaign,
        "recovered <- Cmd.await (Cmd.job (fst attempt))\nCmd.stdout recovered",
    )
    .await;
    assert!(recovered.to_string().contains("first"), "{recovered}");
    for backend in [&backend, &second, &suffix] {
        assert_eq!(
            backend.executions(),
            1,
            "output recovery replayed a command"
        );
    }
})).await;
}

#[tokio::test]
async fn command_wait_preserves_the_exact_continuation_until_terminal_completion() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let running = tokio::spawn(async move {
                    dispatch_haskell_script(
                        policy.as_ref(),
                        &tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/command_wait_continuation.hs",
                        ),
                    )
                    .await
                });
                let backend = TestCommands::new();
                *backend.stdout.lock() = "first".into();
                backend_request(campaign).await.supply(Ok(backend.clone()));
                tokio::time::timeout(Duration::from_secs(30), async {
                    while backend.executions() == 0 {
                        assert!(
                            !running.is_finished(),
                            "command program ended before execution"
                        );
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("command was not started");
                assert!(
                    !running.is_finished(),
                    "terminal wait returned for a running command"
                );
                assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), backend_request(campaign))
                        .await
                        .is_err(),
                    "the suffix started before the first command finished"
                );
                backend.finish();
                let second = TestCommands::completed("second");
                backend_request(campaign).await.supply(Ok(second.clone()));
                let suffix = TestCommands::completed("suffix");
                backend_request(campaign).await.supply(Ok(suffix.clone()));
                let result = running.await.unwrap();
                assert_eq!(result["status"], "committed", "{result}");
                for item in result["items"].as_array().unwrap() {
                    assert_eq!(item["status"], "committed", "{result}");
                }
                let text = result.to_string();
                for marker in ["first", "second", "suffix", "suffix-resumed"] {
                    assert!(text.contains(marker), "missing {marker}: {text}");
                }
                assert_eq!(backend.specs.lock()[0].argv[4], "printf first");
                assert_eq!(second.specs.lock()[0].argv[4], "printf second");
                assert_eq!(suffix.specs.lock()[0].argv[4], "printf suffix");
                for backend in [&backend, &second, &suffix] {
                    assert_eq!(backend.executions(), 1, "continuation replayed a command");
                }
            })
        })
        .await;
}

#[tokio::test]
async fn command_handler_returns_typed_output_failure_without_interactive_recovery() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    committed(
        campaign,
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/command_handler_wait.hs"),
    )
    .await;
    let policy = campaign.root_installation.policy.clone();
    let mut running = tokio::spawn(async move {
        super::tests::dispatch_haskell_script_result(policy.as_ref(),
            "handled <- R.call (execute (R.client handler)) ()\n(Cmd.commandOutcome (Cmd.commandResult handled), Cmd.capturedOutput handled, Cmd.stdout handled, Cmd.stderr handled)\n\"handler-suffix-resumed\" :: Text",
        ).await
    });
    let backend = TestCommands::completed("handler");
    backend
        .output_unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    let request = tokio::select! {
        result = &mut running => panic!("handler ended before requesting backend: {result:?}"),
        request = backend_request(campaign) => request,
    };
    request.supply(Ok(backend.clone()));
    let result = running
        .await
        .unwrap()
        .expect("output availability is a typed handler result");
    assert_eq!(result["status"], "committed", "{result}");
    let typed = result["items"][1]["output"].as_str().unwrap();
    assert!(typed.contains("CommandExited 0"), "{result}");
    assert!(typed.contains("CommandUnavailable"), "{result}");
    assert_eq!(typed.matches("OutputUnavailable").count(), 2, "{result}");
    assert!(
        result.to_string().contains("handler-suffix-resumed"),
        "{result}"
    );
    assert!(!result.to_string().contains("Continue with:"), "{result}");
    assert_eq!(backend.executions(), 1);
    committed(campaign, "21 + 21 :: Int").await;
})).await;
}

#[tokio::test]
async fn command_wait_values_and_explicit_observations_have_one_display_owner() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let mut running = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            &tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/command_presentation.hs",
            ),
        )
        .await
    });
    let backend = TestCommands::new();
    backend.finish.send_replace(true);
    for _ in 0..3 {
        let request = tokio::select! {
            result = &mut running => panic!("command program ended before its expected launches: {result:?}"),
            request = backend_request(campaign) => request,
        };
        request.supply(Ok(backend.clone()));
    }
    let result = running.await.unwrap();
    assert_eq!(result["status"], "committed", "{result}");
    let text = |index: usize| result["items"][index]["output"].as_str().unwrap();
    // A bound command statement summarizes instead of presenting output.
    assert!(text(0).contains("exit 0 · stdout 6 bytes"), "{result}");
    assert!(!text(0).contains("stdout ·"), "{result}");
    assert!(!text(1).contains("stdout ·"), "{result}");
    assert_eq!(text(2).matches("stdout ·").count(), 1, "{result}");
    // The finished-command status/next block renders exactly once, not once
    // from an implicit command presentation and again from ordinary value display.
    assert_eq!(
        text(2).matches("next: inspect outcome and output").count(),
        1,
        "status block renders once: {result}"
    );
    assert!(text(3).contains("Right \"result\""), "{result}");
    assert!(
        !text(4).contains("stdout ·"),
        "bound await presented output: {result}"
    );
    assert!(
        !text(4).contains("retained as"),
        "await minted a presentation binding: {result}"
    );
    assert!(text(5).contains("Right \"result\""), "{result}");
    assert_eq!(
        text(6).matches("stdout ·").count(),
        1,
        "explicit observation presents output exactly once: {result}"
    );
    assert_eq!(
        text(6).matches("next: inspect outcome and output").count(),
        1,
        "observation status block renders once: {result}"
    );
    assert!(
        text(7).contains("result"),
        "explicit page output was consumed: {result}"
    );
    assert_eq!(backend.specs.lock().len(), 3);
})).await;
}

#[tokio::test]
async fn command_skill_examples_execute_in_the_resident_workbench() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let skill = include_str!(
        "../../../../exomonad/examples/workspace/.exomonad/skills/exomonad-command/SKILL.md"
    );
                let mut examples = skill
                    .split("```haskell\n")
                    .skip(1)
                    .map(|block| block.split_once("```").unwrap().0);
                let policy = campaign.root_installation.policy.clone();
                let first = examples.next().unwrap().to_owned();
                let mut running =
                    tokio::spawn(
                        async move { dispatch_haskell_script(policy.as_ref(), &first).await },
                    );
                let backend = TestCommands::new();
                backend.finish.send_replace(true);
                tokio::select! {
                    request = backend_request(campaign) => request.supply(Ok(backend.clone())),
                    result = &mut running => panic!("skill failed before launching: {result:?}"),
                }
                let first = running.await.unwrap();
                assert_eq!(first["status"], "committed", "{first}");
                let result = committed(campaign, examples.next().unwrap()).await;
                assert!(result.to_string().contains("result"), "{result}");
                // A Haskell command without `withMemory` keeps the `Cmd` default; the
                // 1024 MiB default belongs to the direct `bash` tool.
                assert_eq!(backend.specs.lock()[0].memory, 256 * 1024 * 1024);
                assert!(backend.specs.lock()[0].environment.is_empty());
                let description = committed(campaign, examples.next().unwrap()).await;
                assert!(
                    description.to_string().contains("a path; not shell syntax"),
                    "{description}"
                );
                committed(campaign, examples.next().unwrap()).await;

                // Background commands: start and watch, then refuse or accept a pass by
                // the exact commit it ran at.
                committed(campaign, examples.next().unwrap()).await;
                let commit = "0123456789abcdef0123456789abcdef01234567";
                backend_request(campaign)
                    .await
                    .supply(Ok(TestCommands::completed("test result: ok")));
                campaign
                    .next_deployment(
                        "check watch",
                        Duration::from_secs(30),
                        |event| match event {
                            LocalResidentDeployment::WatchChanged { notification }
                                if notification.label == "check-done" =>
                            {
                                Ok(notification)
                            }
                            other => Err(other),
                        },
                    )
                    .await;
                committed(campaign, examples.next().unwrap()).await;
                let accepted = examples.next().unwrap();
                let current = committed(
                    campaign,
                    &format!("let candidate = \"{commit}\" :: Text\n{accepted}"),
                )
                .await;
                assert_eq!(current["items"][2]["output"], "True", "{current}");
                let stale = committed(
                    campaign,
                    &format!("let candidate = \"{}\" :: Text\n{accepted}", "f".repeat(40)),
                )
                .await;
                assert_eq!(stale["items"][2]["output"], "False", "{stale}");
                assert!(
                    examples.next().is_none(),
                    "new skill examples need execution coverage"
                );
            })
        })
        .await;
}

#[tokio::test]
#[ignore = "manual resident declaration latency probe; no subprocess execution"]
async fn command_description_latency_probe() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                for (label, source) in [
                    ("argv-first", "let a = Cmd.argv [\"printf\", \"one\"]"),
                    ("quote-first", "let b = [bash|printf two|]"),
                    ("quote-second", "let c = [bash|printf three|]"),
                    ("argv-second", "let d = Cmd.argv [\"printf\", \"four\"]"),
                    ("reuse", "Cmd.describe b"),
                ] {
                    let start = std::time::Instant::now();
                    committed(campaign, source).await;
                    eprintln!(
                        "command-description {label} elapsed_ms={}",
                        start.elapsed().as_millis()
                    );
                }
            })
        })
        .await;
}

#[tokio::test]
async fn command_output_failure_preserves_the_existing_authored_job() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    committed(
        campaign,
        "retainedBeforeFailure <- Cmd.start [bash|printf preserved|]\nCmd.detach retainedBeforeFailure",
    )
    .await;
    let backend = TestCommands::new();
    backend
        .output_unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    backend.finish.send_replace(true);
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));
    let outcome = super::tests::dispatch_haskell_script_result(
        campaign.root_installation.policy.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/command_binding_failure.hs",
        ),
    )
    .await;
    let rendered = match outcome {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    };
    assert!(rendered.contains("CommandExited 0"), "{rendered}");
    assert!(rendered.contains("CommandUnavailable"), "{rendered}");
    assert!(rendered.contains("OutputUnavailable"), "{rendered}");
    assert!(!rendered.contains("Continue with:"), "{rendered}");
    assert!(!rendered.contains("automatic binding failed"), "{rendered}");
    let status = committed(campaign, "Cmd.status retainedBeforeFailure").await;
    assert!(status.to_string().contains("CommandExited 0"), "{status}");
    assert_eq!(backend.specs.lock().len(), 1);
    assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
})).await;
}

#[tokio::test]
async fn completed_command_output_survives_a_later_failure_in_the_same_computation() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                committed(
                    campaign,
                    "job <- Cmd.start [bash|printf result|]\nCmd.detach job",
                )
                .await;
                let backend = TestCommands::new();
                backend.finish.send_replace(true);
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let result = super::tests::dispatch_haskell_script_result(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/command_prefix_failure.hs",
                    ),
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
            })
        })
        .await;
}

/// Resident command results for the cross-repository hosted response fixture.
/// Each result crosses the same dispatch boundary as the focused command tests.
pub(crate) async fn result_presentation_cases() -> Vec<(
    &'static str,
    &'static str,
    Result<exomonad_actor::ResidentToolResponse, exomonad_actor::ResidentToolError>,
    Vec<&'static str>,
    Option<bool>,
    bool,
)> {
    let mut cases = Vec::new();

    let campaign = TestCampaign::start().await;
    cases = campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                committed(
                    campaign,
                    "job <- Cmd.start [bash|printf result|]\nCmd.detach job",
                )
                .await;
                let backend = TestCommands::completed("result");
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let prefix = super::test_campaign::dispatch_haskell_script_response(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/command_prefix_failure.hs",
                    ),
                )
                .await;
                assert_eq!(backend.executions(), 1);
                cases.push((
                    "runtime-failure-after-completed-prefix",
                    "command_prefix_failure.hs via resident Haskell dispatch",
                    prefix,
                    vec!["failure-after-command", "stdout ·", "result"],
                    Some(false),
                    false,
                ));
                campaign.observe_shutdown().await.unwrap();

                cases
            })
        })
        .await;
    let campaign = TestCampaign::start_with_shell().await;
    cases = campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let running = tokio::spawn(policy.dispatch_boxed(ToolInvocation {
                    context: None,
                    name: "bash".into(),
                    arguments: ToolArguments::Structured(serde_json::json!({"cmd":"incomplete"})),
                }));
                let backend = TestCommands::completed("retained bytes");
                backend
                    .output_unavailable
                    .store(true, std::sync::atomic::Ordering::Release);
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let incomplete = running.await.unwrap();
                assert_eq!(backend.executions(), 1);
                cases.push((
                    "incomplete-output",
                    "structured bash with unavailable output transport",
                    incomplete,
                    vec![
                        "retained as",
                        "read_output session_id=",
                        "Do not rerun",
                        "Output observation unavailable",
                        "output transport lost",
                    ],
                    None,
                    true,
                ));
                campaign.observe_shutdown().await.unwrap();

                cases
            })
        })
        .await;
    let campaign = TestCampaign::start().await;
    cases = campaign.run_scenario(|campaign| Box::pin(async move {
    committed(
        campaign,
        "retainedBeforeFailure <- Cmd.start [bash|printf preserved|]\nCmd.detach retainedBeforeFailure",
    )
    .await;
    let backend = TestCommands::completed("preserved");
    backend
        .output_unavailable
        .store(true, std::sync::atomic::Ordering::Release);
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));
    let recovery = super::test_campaign::dispatch_haskell_script_response(
        campaign.root_installation.policy.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/command_binding_failure.hs",
        ),
    )
    .await;
    assert_eq!(backend.executions(), 1);
    cases.push((
        "typed-output-unavailable",
        "command_binding_failure.hs via resident Haskell dispatch",
        recovery,
        vec!["CommandExited 0", "CommandUnavailable", "OutputUnavailable"],
        None,
        false,
    ));
    campaign.observe_shutdown().await.unwrap();


        cases
    })).await;
    let campaign = TestCampaign::start_with_shell().await;
    cases = campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let running = tokio::spawn(policy.dispatch_boxed(ToolInvocation {
                    context: None,
                    name: "bash".into(),
                    arguments: ToolArguments::Structured(serde_json::json!({"cmd":"large"})),
                }));
                let backend =
                    TestCommands::completed(&format!("BEGIN\n{}\nEND\n", "λ".repeat(32_000)));
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let large = running.await.unwrap();
                assert_eq!(backend.executions(), 1);
                let exomonad_actor::ResidentToolResponse::Workbench(receipt) =
                    large.as_ref().unwrap()
                else {
                    panic!("named command must return a workbench receipt");
                };
                let large_output = &receipt.items[0].output;
                assert!(large_output.contains("BEGIN") && large_output.contains("END"));
                cases.push((
                    "retained-result-reference-amid-large-output",
                    "structured bash with 64000-byte stdout",
                    large,
                    vec![
                        "retained as",
                        "read_output session_id=",
                        "Do not rerun",
                        "output bytes not displayed",
                    ],
                    None,
                    true,
                ));
                campaign.observe_shutdown().await.unwrap();

                cases
            })
        })
        .await;

    cases
}

#[tokio::test]
async fn flat_input_lifecycle_preserves_partial_acknowledgments() {
    use std::sync::atomic::Ordering::Release;
    let campaign = TestCampaign::start_with_shell().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let call = |name: &str, arguments| {
        policy.dispatch_json_boxed(ToolInvocation {
            context: None,
            name: name.into(),
            arguments: ToolArguments::Structured(arguments),
        })
    };
    let backend = TestCommands::new();
    let started = tokio::spawn(call(
        "bash",
        serde_json::json!({"cmd":"input fixture","stdin":true,"yield_time_ms":0}),
    ));
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));
    let receipt = started.await.unwrap().unwrap();
    let output = receipt["items"][0]["output"].as_str().unwrap();
    let session = output
        .split("session_id: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    backend.fail_input.store(true, Release);
    let failed = call("write_stdin", serde_json::json!({"session_id":session,"chars":"first","close_stdin":true,"yield_time_ms":0})).await.unwrap();
    assert!(failed.to_string().contains("Do not replay"), "{failed}");
    assert_eq!(backend.controls.lock().len(), 1);
    backend.fail_input.store(false, Release);
    backend.fail_close.store(true, Release);
    let partial = call("write_stdin", serde_json::json!({"session_id":session,"chars":"final","close_stdin":true,"yield_time_ms":0})).await.unwrap();
    assert!(
        partial
            .to_string()
            .contains("Backend acknowledged the write; child consumption is unknown"),
        "{partial}"
    );
    assert!(
        partial.to_string().contains("Retry close-only"),
        "{partial}"
    );
    assert_eq!(backend.controls.lock().len(), 3);
    backend.fail_close.store(false, Release);
    for _ in 0..2 {
        let closed = call(
            "write_stdin",
            serde_json::json!({"session_id":session,"close_stdin":true,"yield_time_ms":0}),
        )
        .await
        .unwrap();
        assert!(closed.to_string().contains("Stdin is closed"), "{closed}");
    }
    assert_eq!(
        backend.controls.lock().len(),
        4,
        "repeated close is owner-idempotent"
    );
    let rejected = call(
        "write_stdin",
        serde_json::json!({"session_id":session,"chars":"never","yield_time_ms":0}),
    )
    .await
    .unwrap();
    assert!(
        rejected.to_string().contains("stdin is closed"),
        "{rejected}"
    );
    assert!(
        rejected
            .to_string()
            .contains("input not submitted (including chars)"),
        "{rejected}"
    );
    assert!(
        !rejected.to_string().contains("Do not replay"),
        "{rejected}"
    );
    assert_eq!(backend.controls.lock().len(), 4);
    for _ in 0..2 {
        let cancelled = call(
            "cancel_command",
            serde_json::json!({"session_id":session,"yield_time_ms":1000}),
        )
        .await
        .unwrap();
        assert!(
            cancelled.to_string().contains("CommandCancelled"),
            "{cancelled}"
        );
    }
    let retained = call("read_output", serde_json::json!({"session_id":session}))
        .await
        .unwrap();
    assert!(retained.to_string().contains("result"), "{retained}");
})).await;
}

#[tokio::test]
async fn resident_print_preserves_order_and_output_before_same_unit_failure() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let plain = committed(
                    campaign,
                    "print (Just (Right (\"λ line\\nsecond\" :: Text) :: Either Text Text))",
                )
                .await;
                assert!(plain.to_string().contains("λ line"), "{plain}");
                committed(
                    campaign,
                    "job <- Cmd.start [bash|printf result|]\nCmd.detach job",
                )
                .await;
                let backend = TestCommands::completed("command-middle");
                backend_request(campaign).await.supply(Ok(backend));
                let result = super::tests::dispatch_haskell_script_result(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/print_command_failure.hs",
                    ),
                )
                .await;
                let text = match result {
                    Ok(value) => value.to_string(),
                    Err(error) => error.to_string(),
                };
                for marker in [
                    "printed-before",
                    "command-middle",
                    "printed-after",
                    "failure-after-print",
                ] {
                    assert!(text.contains(marker), "missing {marker}: {text}");
                }
                assert!(
                    text.find("printed-before").unwrap() < text.find("command-middle").unwrap(),
                    "{text}"
                );
                assert!(
                    text.find("command-middle").unwrap() < text.find("printed-after").unwrap(),
                    "{text}"
                );
                committed(campaign, "traverse print ([1,2,3] :: [Int])").await;
                let large = committed(campaign, "print (Just (T.replicate 20000 \"λ\"))").await;
                let printed = large["items"][0]["output"].as_str().unwrap();
                assert!(printed.contains("λ"), "{large}");
                assert!(
                    printed.len() < 18000,
                    "bounded Display must not dump the whole value"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn flat_output_pending_is_distinct_from_empty_and_failure() {
    use std::sync::atomic::Ordering::Release;
    let campaign = TestCampaign::start_with_shell().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let call = |name: &str, arguments| {
                    policy.dispatch_json_boxed(ToolInvocation {
                        context: None,
                        name: name.into(),
                        arguments: ToolArguments::Structured(arguments),
                    })
                };
                let backend = TestCommands::new();
                backend.output_pending.store(true, Release);
                let started = tokio::spawn(call(
                    "bash",
                    serde_json::json!({"cmd":"pending fixture","yield_time_ms":0}),
                ));
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let receipt = started.await.unwrap().unwrap();
                let output = receipt["items"][0]["output"].as_str().unwrap();
                assert!(output.contains("No output yet"), "{output}");
                assert!(!output.contains("Unavailable"), "{output}");
                let session = output
                    .split("session_id: ")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .to_owned();
                let pending = call("read_output", serde_json::json!({"session_id":session}))
                    .await
                    .unwrap();
                assert!(pending.to_string().contains("No output yet"), "{pending}");
                backend.output_pending.store(false, Release);
                *backend.stdout.lock() = String::new();
                let empty = call("read_output", serde_json::json!({"session_id":session}))
                    .await
                    .unwrap();
                assert!(empty.to_string().contains("bytes 0–0"), "{empty}");
                assert!(!empty.to_string().contains("No output yet"), "{empty}");
                backend.output_unavailable.store(true, Release);
                let failed = call(
                    "write_stdin",
                    serde_json::json!({"session_id":session,"yield_time_ms":0}),
                )
                .await
                .unwrap();
                assert!(
                    failed.to_string().contains("output transport lost"),
                    "{failed}"
                );
                let unauthorized =
                    call("read_output", serde_json::json!({"session_id":"not-owned"}))
                        .await
                        .unwrap();
                assert!(
                    unauthorized.to_string().contains("unknown command job"),
                    "{unauthorized}"
                );
                backend.finish.send_replace(true);
            })
        })
        .await;
}

#[tokio::test]
async fn command_receipts_preserve_owner_settlement_across_continuation_failure() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let rejected = super::tests::dispatch_haskell_script_result(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/command_receipt_rejected.hs",
                    ),
                )
                .await
                .unwrap_err()
                .to_string();
                assert!(rejected.contains("Rejected (command job)"), "{rejected}");
                assert!(
                    rejected.contains("no job created, no cleanup required"),
                    "{rejected}"
                );
                assert!(!rejected.contains("Unknown (command job)"), "{rejected}");
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), backend_request(campaign))
                        .await
                        .is_err()
                );
                let failed = super::tests::dispatch_haskell_script_result(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/command_receipt_continuation.hs",
                    ),
                )
                .await
                .unwrap_err()
                .to_string();
                assert!(failed.contains("Committed (command job)"), "{failed}");
                assert!(!failed.contains("Unknown (command job)"), "{failed}");
                backend_request(campaign)
                    .await
                    .supply(Ok(TestCommands::completed("started-once")));
            })
        })
        .await;
}

#[tokio::test]
async fn flat_pty_eof_rejection_proves_no_input_submitted() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let call = |name: &str, arguments| {
        policy.dispatch_json_boxed(ToolInvocation {
            context: None,
            name: name.into(),
            arguments: ToolArguments::Structured(arguments),
        })
    };
    let backend = TestCommands::new();
    let pending = tokio::spawn(call(
        "bash",
        serde_json::json!({"cmd":"tty fixture","tty":true,"yield_time_ms":0}),
    ));
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));
    let receipt = pending.await.unwrap().unwrap();
    let text = receipt["items"][0]["output"].as_str().unwrap();
    let session = text
        .split("session_id: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let rejected = call(
        "write_stdin",
        serde_json::json!({"session_id":session,"chars":"hello\n","close_stdin":true}),
    )
    .await
    .unwrap();
    let text = rejected.to_string();
    assert!(
        text.contains("Rejected · input not submitted (including chars); EOF not submitted"),
        "{text}"
    );
    assert!(!text.contains("Do not replay"), "{text}");
    assert!(backend.controls.lock().is_empty());
    call(
        "write_stdin",
        serde_json::json!({"session_id":session,"chars":"hello\n","yield_time_ms":0}),
    )
    .await
    .unwrap();
    assert!(
        matches!(&backend.controls.lock()[..], [CommandControl::Input(text)] if text == "hello\n")
    );
    let cancelled = call(
        "cancel_command",
        serde_json::json!({"session_id":session,"yield_time_ms":1000}),
    )
    .await
    .unwrap();
    assert!(
        cancelled.to_string().contains("cleanup: clean"),
        "{cancelled}"
    );
})).await;
}

async fn command_settlement(campaign: &mut TestCampaign) -> exomonad_actor::SettlementNotification {
    campaign
        .next_deployment(
            "command settlement notice",
            Duration::from_secs(30),
            |event| match event {
                LocalResidentDeployment::SettlementChanged { notification }
                    if notification.command_job.is_some() =>
                {
                    Ok(notification)
                }
                other => Err(other),
            },
        )
        .await
}

#[tokio::test]
async fn structured_bash_explicit_yield_retains_owned_job_without_completion_notice() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let running = tokio::spawn(policy.clone().dispatch_json_boxed(ToolInvocation {
                    context: None,
                    name: "bash".into(),
                    arguments: ToolArguments::Structured(
                        serde_json::json!({"cmd":"long-running","yield_time_ms":0}),
                    ),
                }));
                let backend = TestCommands::new();
                backend_request(campaign).await.supply(Ok(backend.clone()));
                let observed = running.await.unwrap().unwrap();
                assert_eq!(observed["status"], "committed", "{observed}");
                let binding = observed["items"][0]["installedBindings"][0]
                    .as_str()
                    .unwrap();
                let status = committed(campaign, &format!("Cmd.status {binding}")).await;
                assert!(!status.to_string().contains("CommandFinished"), "{status}");
                assert!(
                    !backend.cancelled.load(std::sync::atomic::Ordering::Acquire),
                    "yield lost the live job"
                );
                let output = observed["items"][0]["output"].as_str().unwrap();
                assert!(!output.contains("completion notice"), "{observed}");
                let session = output
                    .split("session_id: ")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap();
                let cancelled = policy
                    .dispatch_json_boxed(ToolInvocation {
                        context: None,
                        name: "cancel_command".into(),
                        arguments: ToolArguments::Structured(
                            serde_json::json!({"session_id":session,"yield_time_ms":1000}),
                        ),
                    })
                    .await
                    .unwrap();
                assert!(
                    cancelled.to_string().contains("CommandCancelled"),
                    "{cancelled}"
                );
                assert_eq!(
                    backend.executions(),
                    1,
                    "cancellation replaced the retained command"
                );
                assert!(
                    campaign
                        .next_deployment_opt(Duration::from_millis(100), |event| match event {
                            LocalResidentDeployment::SettlementChanged { notification }
                                if notification.command_job.is_some() =>
                                Ok(notification),
                            other => Err(other),
                        })
                        .await
                        .is_none(),
                    "an explicit yield armed a completion notice"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn structured_bash_default_waits_until_terminal_without_completion_notice() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let mut running = tokio::spawn(policy.dispatch_json_boxed(ToolInvocation {
                    context: None,
                    name: "bash".into(),
                    arguments: ToolArguments::Structured(serde_json::json!({"cmd":"long-running"})),
                }));
                let backend = TestCommands::new();
                *backend.stdout.lock() = "default-terminal-output".into();
                backend_request(campaign).await.supply(Ok(backend.clone()));
                tokio::time::timeout(Duration::from_secs(30), async {
                    while backend.executions() == 0 {
                        assert!(
                            !running.is_finished(),
                            "default bash ended before execution"
                        );
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("command was not started");
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), &mut running)
                        .await
                        .is_err(),
                    "default bash returned for a running command"
                );
                assert!(!backend.cancelled.load(std::sync::atomic::Ordering::Acquire));
                backend.finish();
                let observed = running.await.unwrap().unwrap();
                assert_eq!(observed["status"], "committed", "{observed}");
                let output = observed["items"][0]["output"].as_str().unwrap();
                assert!(output.contains("CommandExited 0"), "{observed}");
                assert_eq!(
                    output.matches("default-terminal-output").count(),
                    1,
                    "default bash presented output more than once: {observed}"
                );
                assert!(!output.contains("completion notice"), "{observed}");
                let binding = observed["items"][0]["installedBindings"][0]
                    .as_str()
                    .unwrap();
                let retained = committed(campaign, &format!("Cmd.status {binding}")).await;
                assert!(
                    retained.to_string().contains("CommandExited 0"),
                    "{retained}"
                );
                assert_eq!(backend.executions(), 1);
                assert!(
                    campaign
                        .next_deployment_opt(Duration::from_millis(100), |event| match event {
                            LocalResidentDeployment::SettlementChanged { notification }
                                if notification.command_job.is_some() =>
                                Ok(notification),
                            other => Err(other),
                        })
                        .await
                        .is_none(),
                    "default bash armed a completion notice"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn sibling_actor_progresses_during_foreground_command_wait() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    committed(
        campaign,
        "job <- Cmd.start [bash|long-running|]\nCmd.detach job",
    )
    .await;
    let backend = TestCommands::new();
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));

    let launch = {
        let root = campaign.root_installation.policy.clone();
        tokio::spawn(async move {
            dispatch_haskell_script(
                root.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/inherited_command_observer.hs",
                ),
            )
            .await
        })
    };
    let child = campaign
        .next_deployment(
            "sibling actor installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        child.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
            integrate: true,
        },
    );
    let _custody = child
        .worktree_custody
        .clone()
        .expect("child checkout binding remains live for this test");
    campaign.acknowledge_native_spawn(&child);
    require_committed(&launch.await.unwrap());
    campaign
        .next_deployment(
            "sibling actor ready",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;

    let foreign = super::tests::dispatch_haskell_script_result(
        child.policy.as_ref(),
        "Cmd.observeCompletion (Cmd.Observation 0 1024) job",
    )
    .await;
    let rendered = match foreign {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    };
    assert!(rendered.contains("not authorized"), "{rendered}");
    campaign.assert_no_deployment("foreign observation armed an owner notice", |event| {
        matches!(event, LocalResidentDeployment::SettlementChanged { notification } if notification.command_job.is_some())
    });

    let policy = campaign.root_installation.policy.clone();
    let waiting = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            "Cmd.observeCompletion (Cmd.Observation 30000 1024) job",
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if matches!(
                campaign
                    .root_installation
                    .runtime_observation
                    .snapshot()
                    .workbench_posture,
                exomonad_actor::ActorWorkbenchPosture::AwaitingEffect { effect, .. }
                    if effect == "command job"
            ) {
                break;
            }
            assert!(
                !waiting.is_finished(),
                "command observation ended before its wait effect"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("command wait effect was not reached");

    let sibling = tokio::time::timeout(
        Duration::from_secs(10),
        dispatch_haskell_script(child.policy.as_ref(), "40 + 2 :: Int"),
    )
    .await
    .expect("sibling actor did not progress while the command waited");
    assert_eq!(sibling["status"], "committed", "{sibling}");
    assert!(sibling.to_string().contains("42"), "{sibling}");
    assert!(
        !waiting.is_finished(),
        "command wait ended before sibling progress"
    );
    backend.finish();
    let observed = waiting.await.unwrap();
    assert_eq!(observed["status"], "committed", "{observed}");
    assert_eq!(backend.executions(), 1);
})).await;
}

#[tokio::test]
async fn background_bash_returns_at_once_and_its_notice_carries_the_source() {
    let campaign = TestCampaign::start_with_shell().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let running = tokio::spawn(policy.dispatch_json_boxed(ToolInvocation {
        context: None,
        name: "bash".into(),
        arguments: ToolArguments::Structured(serde_json::json!({
            "cmd":"cargo test -p crate --lib","background":true,
            "yield_time_ms":-1,"max_output_bytes":0,"focus":"ignored in background"
        })),
    }));
    // The source probe is released first; the command waits behind it. The
    // probe is answered while the call is in flight so its admission bound
    // cannot expire behind a slow compile of the call itself.
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let probe = TestCommands::completed_streams(&format!("/work/tree\n{commit}\ndirty\n"), "");
    let request = raw_backend_request(campaign).await;
    assert_eq!(request.purpose, CommandBackendPurpose::SourceProbe);
    request.supply(Ok(probe.clone()));
    // The call returns without the command's backend: nothing waits on it.
    let response = running.await.unwrap().unwrap();
    assert_eq!(response["status"], "committed", "{response}");
    let output = response["items"][0]["output"].as_str().unwrap();
    assert!(
        output.contains("running in background; its completion notice will wake you")
            && output.contains("A host restart loses a running job."),
        "{output}"
    );
    let binding = response["items"][0]["installedBindings"][0]
        .as_str()
        .unwrap();
    assert!(
        output.contains(&format!("retained as {binding} :: Cmd.Job")),
        "{output}"
    );
    let job = output
        .split("session_id: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();

    let command = TestCommands::completed_streams("running 3 tests\ntest result: ok\n", "");
    backend_request(campaign)
        .await
        .supply(Ok(command.clone()));
    // The command is released only once the probe has finished.
    assert!(probe.specs.lock()[0].argv[4].contains("git rev-parse"));

    let notice = command_settlement(campaign).await;
    assert_eq!(command.specs.lock()[0].argv[4], "cargo test -p crate --lib");
    assert_eq!(notice.command_job.as_deref(), Some(job.as_str()));
    assert_eq!(notice.target_revision.as_deref(), Some(commit));
    let rendered = notice.reply_preview.expect("settled command report");
    assert_eq!(
        rendered,
        format!(
            "command: cargo test -p crate --lib\nexit 0 · process and cleanup terminal · output complete\nstarted in /work/tree at {commit} with uncommitted changes\nstdout tail:\nrunning 3 tests\ntest result: ok\nFull output: read_output session_id={job}; nothing reruns. The checkout was not guarded while it ran; a pass covers this source only, not a later revision."
        )
    );
})).await;
}

#[tokio::test]
async fn command_direct_wait_captures_report_and_releases_subscription() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let running = tokio::spawn(async move {
                    dispatch_haskell_script(
                        policy.as_ref(),
                        &tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/command_direct_wait.hs",
                        ),
                    )
                    .await
                });
                let backend = TestCommands::new();
                backend_request(campaign).await.supply(Ok(backend.clone()));
                tokio::time::timeout(Duration::from_secs(30), async {
                    while backend.executions() == 0 {
                        assert!(!running.is_finished(), "direct wait ended before execution");
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("command was not started");
                assert!(
                    !running.is_finished(),
                    "direct wait returned before terminal completion"
                );
                backend.finish();
                let result = running.await.unwrap();
                assert_eq!(result["status"], "committed", "{result}");
                let text = result.to_string();
                for marker in [
                    "CommandExited 0",
                    "0123456789abcdef0123456789abcdef01234567",
                    "Just",
                    "True",
                    "direct-wait-captured",
                ] {
                    assert!(text.contains(marker), "missing {marker}: {text}");
                }
                assert!(
                    campaign
                        .next_deployment_opt(Duration::from_millis(100), |event| match event {
                            LocalResidentDeployment::WatchChanged { notification } =>
                                Ok(notification),
                            other => Err(other),
                        })
                        .await
                        .is_none(),
                    "a direct wait emitted a named-watch notice"
                );
                assert_eq!(backend.executions(), 1);
            })
        })
        .await;
}

#[tokio::test]
async fn watched_command_jobs_wake_once_with_a_typed_report() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    committed(
        campaign,
        "check <- Cmd.background (Cmd.withSource [bash|cargo test|])\nquick <- Cmd.start (Cmd.withSource [bash|true|])\nCmd.detach quick\nchecked <- watch \"check-done\" ((,) <$> Cmd.awaitFinished check <*> Cmd.awaitFinished quick)",
    )
    .await;
    let commit = "89abcdef0123456789abcdef0123456789abcdef";
    let commands = TestCommands::completed_streams("", "error[E0308]: mismatched types\n");
    *commands.script_exit_code.lock() = Some(("cargo test".into(), 101));
    let mut source_probes = 0;
    let mut command_requests = 0;
    while source_probes < 2 || command_requests < 2 {
        let request = raw_backend_request(campaign).await;
        match request.purpose {
            CommandBackendPurpose::SourceProbe => {
                source_probes += 1;
                request.supply(Ok(TestCommands::completed(&format!(
                    "/work/tree\n{commit}\nclean\n"
                ))));
            }
            CommandBackendPurpose::Command => {
                command_requests += 1;
                request.supply(Ok(commands.clone()));
            }
        }
    }
    assert_eq!(source_probes, 2);
    assert_eq!(command_requests, 2);
    campaign
        .next_deployment(
            "watch notice",
            Duration::from_secs(30),
            |event| match event {
                LocalResidentDeployment::WatchChanged { notification }
                    if notification.label == "check-done" =>
                {
                    Ok(notification)
                }
                other => Err(other),
            },
        )
        .await;
    // The owner's watch took the settlement wake over: no second notice.
    assert!(campaign
        .next_deployment_opt(Duration::from_millis(500), |event| match event {
            LocalResidentDeployment::SettlementChanged { notification } => Ok(notification),
            other => Err(other),
        })
        .await
        .is_none());
    let observed = committed(
        campaign,
        "WatchReady (report, foreground) <- pollWatch checked\n(Cmd.commandOutcome (Cmd.reportResult report), fmap Cmd.sourceCommit (Cmd.reportSource report), Cmd.reportOutputComplete report, fmap Cmd.sourceCommit (Cmd.reportSource foreground))",
    )
    .await;
    let text = observed["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap();
    assert!(text.contains("CommandExited 101"), "{text}");
    assert!(text.contains(commit), "{text}");
    assert!(text.contains("True"), "{text}");
    assert_eq!(
        text.matches(commit).count(),
        2,
        "both commands carry source: {text}"
    );
    assert!(!text.contains("Nothing"), "{text}");
    assert_eq!(commands.executions(), 2);
})).await;
}

#[tokio::test]
async fn a_job_finished_before_its_watch_registers_wakes_exactly_once() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let running = tokio::spawn({
        let policy = campaign.root_installation.policy.clone();
        async move {
            dispatch_haskell_script(
                policy.as_ref(),
                "done <- Cmd.start [bash|true|]\nCmd.await done",
            )
            .await
        }
    });
    backend_request(campaign)
        .await
        .supply(Ok(TestCommands::completed("")));
    assert_eq!(running.await.unwrap()["status"], "committed");
    // Twice: a second watch on the same finished job is served as well.
    for label in ["late-one", "late-two"] {
        committed(
            campaign,
            &format!("w <- watch \"{label}\" (Cmd.awaitFinished done)\nWatchReady r <- pollWatch w\nCmd.commandOutcome (Cmd.reportResult r)"),
        )
        .await;
        campaign
            .next_deployment("late watch", Duration::from_secs(30), |event| match event {
                LocalResidentDeployment::WatchChanged { notification }
                    if notification.label == label =>
                {
                    Ok(notification)
                }
                other => Err(other),
            })
            .await;
    }
    assert!(campaign
        .next_deployment_opt(Duration::from_millis(500), |event| match event {
            LocalResidentDeployment::WatchChanged { notification } => Ok(notification),
            LocalResidentDeployment::SettlementChanged { notification } =>
                Err(LocalResidentDeployment::SettlementChanged { notification }),
            other => Err(other),
        })
        .await
        .is_none());
    campaign.assert_no_deployment("no settlement notice for a watched job", |event| {
        matches!(event, LocalResidentDeployment::SettlementChanged { .. })
    });
})).await;
}
