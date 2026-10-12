//! Startup acceptance through the configured native host and its provider seam.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    commit_workspace, committed_display_text, hosted_script_provider, hosted_test_settings,
    next_hosted_script_round, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
};
use exomonad_actor::{InitialContextObservation, InitialContextState, JevCallFailure};
use harness::transport::ResponsesRequest;
use serde_json::Value;
use std::{collections::VecDeque, sync::Arc, time::Duration};
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
async fn startup_gates_queued_input_and_refreshes_exact_reference() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut judgments) = mpsc::unbounded_channel();
    let (assembly, mut assembled) = oneshot::channel();
    let mut starting = Box::pin(HostedTestRuntime::start_observing_initialization(
        &settings,
        &provider,
        move |config| {
            configure_startup(config, "agentSpec", "runtime-memory-first");
            config.jev = Some(Arc::new(StartupJevGate(entered)));
        },
        assembly,
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
    let context = assembled
        .try_recv()
        .expect("startup effects run after actual production assembly");
    assert_eq!(
        context
            .forest
            .initial_context_observation(context.actor.identity())
            .unwrap()
            .state,
        InitialContextState::Preparing
    );
    let target = context
        .binding(context.actor.identity())
        .expect("preparing root retains its exact pending binding")
        .identity()
        .clone();
    let input_operation = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
    context
        .while_root_live(
            "queue input during startup",
            context.http_command(
                input_operation,
                harness::server::HostCommand::Input {
                    target: target.clone(),
                    text: "startup-activation".into(),
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
    let store = context.runtime.store();
    let pending_input = store
        .embedded_command(&target.run, input_operation)
        .unwrap()
        .expect("HTTP input is retained by the real command owner");
    assert!(pending_input.receipt.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), requests.recv())
            .await
            .is_err()
    );
    assert!(store
        .embedded_command(&target.run, input_operation)
        .unwrap()
        .unwrap()
        .receipt
        .is_none());
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

#[tokio::test(flavor = "multi_thread")]
async fn fresh_children_initialize_idle_and_captured_children_skip_the_hook() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut judgments) = mpsc::unbounded_channel();
    let host = HostedTestRuntime::start_configured(&settings, &provider, move |config| {
        configure_startup(config, "absentSpec", "child-runtime-memory");
        config.jev = Some(Arc::new(StartupJevGate(entered)));
    })
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            assert_eq!(observation(host).state, InitialContextState::AbsentHook);
            host.input("install-startup-children").await.unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = VecDeque::new();
            let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let operation = first.operation("startup-children-spawn");
            first.sync_call(
                "startup-children-spawn",
                include_str!("fixtures/startup_children_spawn.hs"),
            );
            let mut operations = Vec::new();
            for caller in ["fresh-caller-seed", ""] {
                let held = host
                    .while_operation_succeeds(
                        &operation,
                        tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, judgments.recv()),
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(held.packet["caller_seed_present"], true);
                assert_eq!(held.packet["caller_seed"], caller);
                assert_eq!(held.packet["memory"], "child-runtime-memory");
                operations.push(held.packet["operation"].as_str().unwrap().to_owned());
                let node = host
                    .context
                    .forest
                    .inspect_host_graph()
                    .into_iter()
                    .find(|node| {
                        host.context
                            .forest
                            .initial_context_observation(node.actor)
                            .ok()
                            .and_then(|observation| observation.identity)
                            .is_some_and(|identity| {
                                identity.operation.as_str() == operations.last().unwrap()
                            })
                    })
                    .expect("operation was issued by the fresh child owner");
                assert_ne!(node.actor, host.context.actor.identity());
                assert_eq!(node.creator, Some(host.context.actor.identity()));
                assert!(host
                    .context
                    .binding(node.actor)
                    .unwrap()
                    .conversation()
                    .is_none());
                assert!(matches!(
                    requests.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
                held.answer.send(true).unwrap();
            }
            assert_ne!(operations[0], operations[1]);
            let installed = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            installed.assert_value("startup-children-spawn", "True");
            assert!(pending.is_empty(), "initialized children remain idle");
            assert!(matches!(
                requests.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            assert!(matches!(
                judgments.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            let graph = host.context.forest.inspect_host_graph();
            let child = |label: &str| graph.iter().find(|node| node.label == label).unwrap();
            for label in [
                "startup-fresh",
                "startup-empty-caller",
                "startup-captured",
                "startup-empty-result",
                "startup-absent",
            ] {
                let node = child(label);
                assert!(node.active_requests.is_empty());
                assert!(node.queued_requests.is_empty());
                assert!(node.terminal().is_none());
                assert!(host
                    .context
                    .binding(node.actor)
                    .unwrap()
                    .conversation()
                    .is_some());
            }
            let fresh = host
                .context
                .forest
                .initial_context_observation(child("startup-fresh").actor)
                .unwrap();
            let empty_caller = host
                .context
                .forest
                .initial_context_observation(child("startup-empty-caller").actor)
                .unwrap();
            assert_eq!(fresh.state, InitialContextState::Published);
            assert_eq!(fresh.caller_seed.as_deref(), Some("fresh-caller-seed"));
            assert_eq!(empty_caller.caller_seed.as_deref(), Some(""));
            assert!(empty_caller
                .publication
                .as_ref()
                .unwrap()
                .caller_reference
                .is_some());
            assert_eq!(
                host.context
                    .forest
                    .initial_context_observation(child("startup-captured").actor)
                    .unwrap()
                    .state,
                InitialContextState::Captured
            );
            let empty = host
                .context
                .forest
                .initial_context_observation(child("startup-empty-result").actor)
                .unwrap();
            assert_eq!(empty.state, InitialContextState::Published);
            assert_eq!(empty.result.as_ref().unwrap().rendered, "");
            assert!(empty
                .publication
                .as_ref()
                .unwrap()
                .authored_reference
                .is_none());
            assert!(empty
                .publication
                .as_ref()
                .unwrap()
                .caller_reference
                .is_some());
            assert_eq!(
                host.context
                    .forest
                    .initial_context_observation(child("startup-absent").actor)
                    .unwrap()
                    .state,
                InitialContextState::AbsentHook
            );
            let path = host
                .context
                .binding(child("startup-fresh").actor)
                .unwrap()
                .identity()
                .actor
                .clone();
            installed.call(
                "startup-child-activate",
                include_str!("fixtures/startup_child_activate.hs"),
            );
            let parent = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            parent.assert_value("startup-child-activate", "True");
            parent.finish();
            let activated = next_hosted_script_round(&mut requests, &mut pending, &path).await;
            let texts = user_texts(&activated.request);
            assert_eq!(texts[0], "fresh-caller-seed");
            assert_eq!(texts[1], fresh.result.as_ref().unwrap().rendered);
            assert!(matches!(
                judgments.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            activated.call(
                "startup-child-reply",
                include_str!("fixtures/startup_child_reply.hs"),
            );
            let replied = next_hosted_script_round(&mut requests, &mut pending, &path).await;
            replied.finish();
        })
    })
    .await;
}

async fn startup_refusal(cancel: bool) {
    let files = tempfile::tempdir().unwrap();
    let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("README.md", "source\n", "seed")
        .unwrap();
    let workspace = repository.path().to_owned();
    let run = files.path().join("refused-startup-run");
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut judgments) = mpsc::unbounded_channel();
    let (assembly, mut assembled) = oneshot::channel();
    let mut starting = Box::pin(HostedTestRuntime::start_observing_initialization(
        &settings,
        &provider,
        move |config| {
            config.workspace = workspace;
            config.run_directory =
                tidepool_atomic_write::DirectoryAnchor::open_existing(run.parent().unwrap())
                    .unwrap()
                    .child(run.file_name().unwrap())
                    .unwrap();
            config.root_binding_path = run.join("root-binding.json");
            configure_startup(config, "agentSpec", "interrupted-startup-memory");
            config.jev = Some(Arc::new(StartupJevGate(entered)));
        },
        assembly,
    ));
    let held = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        tokio::select! {
            judgment = judgments.recv() => judgment.expect("startup reaches real effect dispatch"),
            result = &mut starting => match result {
                Ok(_) => panic!("startup bypassed held judgment"),
                Err(failure) => panic!("startup failed before controlled effect: {failure}"),
            },
        }
    })
    .await
    .unwrap();
    let context = assembled.try_recv().unwrap();
    let identity = context.actor.identity();
    let before = context
        .forest
        .initial_context_observation(identity)
        .unwrap();
    assert_eq!(before.state, InitialContextState::Preparing);
    assert!(context.binding(identity).unwrap().conversation().is_none());
    assert_eq!(
        std::fs::read_to_string(
            context
                .config
                .workspace
                .join(".exomonad/state/startup-witness.txt")
        )
        .unwrap(),
        before.identity.as_ref().unwrap().operation.as_str()
    );
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    if cancel {
        let cleanup = tokio::time::timeout(
            COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
            context
                .actor
                .shutdown_with_cleanup(exomonad_actor::ActorTerminal::new(
                    exomonad_actor::ActorExitKind::Cancelled,
                    "cancel held startup",
                )),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            cleanup.terminal.kind,
            exomonad_actor::ActorExitKind::Cancelled
        );
    } else {
        // Dropping the controlled backend answer makes the real interpreter fail.
        drop(held.answer);
    }
    let failure = match tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, starting)
        .await
        .unwrap()
    {
        Ok(host) => {
            host.stop().await.unwrap();
            panic!("failed startup became ready");
        }
        Err(failure) => failure,
    };
    assert!(failure.cleanup_confirmed(), "{failure}");
    let journal = exomonad_actor::ActorRecoveryJournal::open_existing(
        &context.config.run_directory,
        "actor-lifecycle.v2.jsonl",
    )
    .unwrap();
    let after = journal.initial_context(identity).unwrap().unwrap();
    if cancel {
        assert_eq!(after.state, InitialContextState::Cancelled);
    } else {
        assert!(matches!(after.state, InitialContextState::Failed { .. }));
    }
    assert_eq!(after.identity, before.identity);
    assert!(after.result.is_none());
    assert!(after.publication.is_none());
    assert!(context
        .binding(identity)
        .is_none_or(|binding| binding.conversation().is_none()));
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_startup_effect_refuses_readiness_and_confirms_cleanup() {
    startup_refusal(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_startup_effect_refuses_readiness_and_confirms_cleanup() {
    startup_refusal(true).await;
}
