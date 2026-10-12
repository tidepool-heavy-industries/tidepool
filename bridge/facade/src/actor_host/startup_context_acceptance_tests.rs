//! Startup acceptance through the configured native host and its provider seam.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    commit_workspace, committed_display_text, hosted_script_provider, hosted_test_settings,
    next_hosted_script_round, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
};
use exomonad_actor::{InitialContextObservation, InitialContextState, JevCallFailure};
use harness::transport::ResponsesRequest;
use serde_json::Value;
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::{mpsc, oneshot};

struct HeldJudgment {
    packet: Value,
    answer: oneshot::Sender<bool>,
}

struct StartupJevGate(mpsc::UnboundedSender<HeldJudgment>);

impl exomonad_actor::JevBackend for StartupJevGate {
    fn ask(
        &self,
        packet: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        Box::pin(async move {
            let packet = serde_json::from_str(&packet)
                .map_err(|issue| JevCallFailure::JevMalformed(issue.to_string()))?;
            let (answer, waiting) = oneshot::channel();
            self.0
                .send(HeldJudgment { packet, answer })
                .map_err(|_| JevCallFailure::JevTransport("startup observer closed".into()))?;
            let answer = waiting
                .await
                .map_err(|_| JevCallFailure::JevTransport("startup gate abandoned".into()))?;
            Ok(serde_json::to_string(&answer).unwrap())
        })
    }
}

fn configure_startup(config: &mut super::ActorHostConfig, entry: &str, memory: &str) {
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("state")).unwrap();
    std::fs::write(
        authored.join("AgentSpec.hs"),
        include_str!("fixtures/startup_context_spec.hs"),
    )
    .unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec!["AgentSpec".into()];
        project.haskell.spec = Some(format!("AgentSpec.{entry}"));
    });
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .unwrap(),
    );
    // State is a runtime input written after the selected source is frozen.
    std::fs::write(authored.join("state/startup-memory.txt"), memory).unwrap();
}

fn user_texts(request: &ResponsesRequest) -> Vec<&str> {
    request
        .input
        .iter()
        .filter(|item| item.0["type"] == "message" && item.0["role"] == "user")
        .map(|item| {
            item.0["content"]
                .as_str()
                .expect("startup and activating input are ordinary text messages")
        })
        .collect()
}

fn observation(host: &HostedTestRuntime) -> InitialContextObservation {
    host.context
        .forest
        .initial_context_observation(host.context.actor.identity())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_effects_finish_before_readiness_and_first_request_then_refresh_exact_reference() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut judgments) = mpsc::unbounded_channel();
    let mut starting = Box::pin(HostedTestRuntime::start_configured(
        &settings,
        &provider,
        move |config| {
            configure_startup(config, "agentSpec", "runtime-memory-first");
            config.jev = Some(Arc::new(StartupJevGate(entered)));
        },
    ));
    let held = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        tokio::select! {
            judgment = judgments.recv() => judgment.expect("startup reaches the real Jev interpreter"),
            ready = &mut starting => match ready {
                Ok(_) => panic!("host became ready before its initializer settled"),
                Err(failure) => panic!("host failed before its initializer reached Jev: {failure}"),
            },
        }
    })
    .await
    .expect("startup fixture must compile and reach Jev");
    assert_eq!(held.packet["memory"], "runtime-memory-first");
    assert_eq!(held.packet["caller_seed_present"], false);
    assert_eq!(held.packet["caller_seed"], Value::Null);
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    held.answer.send(true).unwrap();
    let host = starting.await.unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            let retained = observation(host);
            assert_eq!(retained.state, InitialContextState::Published);
            let identity = retained.identity.as_ref().unwrap();
            assert_eq!(identity.actor, host.context.actor.identity());
            assert_eq!(held.packet["operation"], identity.operation.as_str());
            assert_eq!(
                std::fs::read_to_string(
                    host.context
                        .config
                        .workspace
                        .join(".exomonad/state/startup-witness.txt")
                )
                .unwrap(),
                identity.operation.as_str()
            );
            let result = retained.result.as_ref().unwrap();
            assert_eq!(result.sections.len(), 2);
            assert_eq!(result.sections[0].body, "runtime-memory-first");
            assert_eq!(
                result.sections[0].source.as_deref(),
                Some("startup-memory.txt")
            );
            assert_eq!(result.sections[1].body, "included");
            let publication = retained.publication.as_ref().unwrap();
            assert!(publication.authored_reference.is_some());
            assert!(publication.caller_reference.is_none());
            host.input("startup-activation").await.unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = VecDeque::new();
            let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            assert_eq!(
                user_texts(&first.request),
                [result.rendered.as_str(), "startup-activation"]
            );
            assert!(first
                .request
                .input
                .iter()
                .all(|item| item.tool_call().unwrap().is_none()));
            let instructions = first.request.instructions.clone();
            first.call(
                "startup-refresh",
                include_str!("fixtures/startup_context_refresh.hs"),
            );
            let refreshed = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            assert_eq!(
                committed_display_text(&refreshed.settled_output("startup-refresh")),
                "True"
            );
            assert_eq!(
                user_texts(&refreshed.request),
                ["startup-refreshed", "startup-activation"]
            );
            assert_eq!(refreshed.request.instructions, instructions);
            assert_eq!(observation(host), retained);
            refreshed.call(
                "startup-refresh-again",
                include_str!("fixtures/startup_context_refresh_again.hs"),
            );
            let repeated = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            assert_eq!(
                committed_display_text(&repeated.settled_output("startup-refresh-again")),
                "True"
            );
            assert_eq!(
                user_texts(&repeated.request),
                ["startup-refreshed-again", "startup-activation"]
            );
            assert_eq!(repeated.request.instructions, instructions);
            let failed_operation = repeated.operation("startup-refresh-failure");
            repeated.call(
                "startup-refresh-failure",
                include_str!("fixtures/startup_context_refresh_failure.hs"),
            );
            let staged = host
                .while_operation_succeeds(
                    &failed_operation,
                    tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, judgments.recv()),
                )
                .await
                .expect("refresh remains live until its post-edit effect")
                .expect("refresh reaches its post-edit effect")
                .expect("the real Jev observer remains available");
            assert_eq!(staged.packet["startup_refresh_staged"], true);
            staged.answer.send(true).unwrap();
            let failed = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let failure = failed.settled_output("startup-refresh-failure");
            assert_ne!(failure["status"], "committed");
            assert_eq!(
                failure["failure"]["phase"],
                tidepool_toolchain::failclass::Phase::Run.tag(),
                "{failure}"
            );
            assert_eq!(
                user_texts(&failed.request),
                ["startup-refreshed-again", "startup-activation"]
            );
            assert_eq!(failed.request.instructions, instructions);
            assert_eq!(observation(host), retained);
            assert!(matches!(
                judgments.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            failed.finish();
        })
    })
    .await;
}
