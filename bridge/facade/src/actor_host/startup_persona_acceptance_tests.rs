//! The delivered persona carries full authored memory to the first provider
//! request and refreshes that exact issued block only on explicit activation.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    commit_workspace, committed_display_text, hosted_script_provider, hosted_test_settings,
    next_hosted_script_round, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
};
use exomonad_actor::{InitialContextState, JevCallFailure};
use serde_json::{json, Value};
use std::{collections::VecDeque, path::Path, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

struct PersonaJudgment {
    packet: Value,
    answer: oneshot::Sender<Value>,
}

struct PersonaJevGate(mpsc::UnboundedSender<PersonaJudgment>);

impl exomonad_actor::JevBackend for PersonaJevGate {
    fn ask(
        &self,
        packet: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        Box::pin(async move {
            let packet = serde_json::from_str(&packet)
                .map_err(|issue| JevCallFailure::JevMalformed(issue.to_string()))?;
            let (answer, waiting) = oneshot::channel();
            self.0
                .send(PersonaJudgment { packet, answer })
                .map_err(|_| {
                    JevCallFailure::JevTransport("persona judgment observer closed".into())
                })?;
            waiting
                .await
                .map(|answer| answer.to_string())
                .map_err(|_| JevCallFailure::JevTransport("persona judgment abandoned".into()))
        })
    }
}

fn retained_location(config: &mut super::ActorHostConfig, workspace: &Path, run: &Path) {
    config.workspace = workspace.to_owned();
    config.run_directory =
        tidepool_atomic_write::DirectoryAnchor::open_existing(run.parent().unwrap())
            .unwrap()
            .child(run.file_name().unwrap())
            .unwrap();
    config.root_binding_path = run.join("root-binding.json");
}

pub(super) fn configure_persona(config: &mut super::ActorHostConfig, entry: &str) {
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Persona")).unwrap();
    std::fs::create_dir_all(authored.join("Jev")).unwrap();
    let sources = [
        (
            "Persona/Memory.hs",
            tidepool_testing::fixture_source(
                "exomonad/examples/persona/.exomonad/Persona/Memory.hs",
            ),
        ),
        (
            "Persona/Web.hs",
            tidepool_testing::fixture_source("exomonad/examples/persona/.exomonad/Persona/Web.hs"),
        ),
        (
            "Persona/Spec.hs",
            tidepool_testing::fixture_source("exomonad/examples/persona/.exomonad/Persona/Spec.hs"),
        ),
        (
            "Persona/Orientation.hs",
            tidepool_testing::fixture_source(
                "exomonad/examples/persona/.exomonad/Persona/Orientation.hs",
            ),
        ),
        (
            "Persona/Context.hs",
            tidepool_testing::fixture_source(
                "exomonad/examples/persona/.exomonad/Persona/Context.hs",
            ),
        ),
        (
            "Jev/Operators.hs",
            tidepool_testing::fixture_source(
                "exomonad/examples/workspace/.exomonad/Jev/Operators.hs",
            ),
        ),
    ];
    for (relative, source) in sources {
        std::fs::write(authored.join(relative), source).unwrap();
    }
    let conventional_spec = entry == "AgentSpec.agentSpec";
    if conventional_spec {
        std::fs::write(
            authored.join("AgentSpec.hs"),
            tidepool_testing::fixture_source("exomonad/examples/persona/.exomonad/AgentSpec.hs"),
        )
        .unwrap();
    }
    std::fs::write(
        config.workspace.join("flake.nix"),
        include_str!("../../../../exomonad/examples/workspace/flake.nix"),
    )
    .unwrap();
    std::fs::write(
        config.workspace.join("flake.lock"),
        include_str!("../../../../exomonad/examples/workspace/flake.lock"),
    )
    .unwrap();
    std::fs::write(config.workspace.join(".gitignore"), ".exomonad/state/\n").unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec![
            "Persona.Memory".into(),
            "Persona.Web".into(),
            "Persona.Spec".into(),
            "Persona.Orientation".into(),
            "Persona.Context".into(),
            "Jev.Operators".into(),
        ];
        if conventional_spec {
            project.haskell.modules.insert(0, "AgentSpec".into());
        }
        project.haskell.spec = Some(entry.into());
        project
            .haskell
            .flake_sources
            .insert("jev-dsl".into(), vec!["core".into()]);
    });
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .unwrap(),
    );
    let memory = authored.join("state/memory");
    std::fs::create_dir_all(&memory).unwrap();
    for (name, body) in [
        ("persona", "persona-memory-original"),
        (
            "human",
            "A saved correction: answer from supplied memory when asked what you already remember.",
        ),
        ("curiosities", "curiosity-memory-original"),
    ] {
        std::fs::write(memory.join(format!("{name}.md")), body).unwrap();
    }
}

async fn settled_head(host: &HostedTestRuntime) -> harness::model::RequestId {
    host.while_host_running(tokio::time::timeout(
        COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
        async {
            loop {
                let binding = host.context.binding(host.context.actor.identity()).unwrap();
                let conversation = binding.conversation().unwrap();
                let frontier = host
                    .runtime
                    .store()
                    .embedded_round_frontier(conversation.identity())
                    .unwrap();
                if frontier.pending_head.is_none() && conversation.active_round().is_none() {
                    if let Some(head) = frontier.settled_head {
                        return head;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        },
    ))
    .await
    .unwrap()
    .expect("persona scripted round settles")
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_persona_arrives_refreshes_and_retires_without_judgment() {
    let mut files = tempfile::tempdir().unwrap();
    let mut repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("README.md", "source\n", "seed")
        .unwrap();
    let workspace = repository.path().to_owned();
    let run = files.path().join("persona-run");
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut judgments) = mpsc::unbounded_channel();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        retained_location(config, &workspace, &run);
        configure_persona(config, "AgentSpec.agentSpec");
        config.jev = Some(Arc::new(PersonaJevGate(entered)));
    })
    .await
    .unwrap();
    {
        let retained = host
            .context
            .forest
            .initial_context_observation(host.context.actor.identity())
            .await
            .unwrap();
        assert_eq!(retained.state, InitialContextState::Published);
        let result = retained.result.as_ref().unwrap();
        assert_eq!(result.sections.len(), 1);
        let original = ["persona", "human", "curiosities"]
            .map(|name| {
                format!(
                    "[Memory file: .exomonad/state/memory/{name}.md]\n{}",
                    std::fs::read_to_string(
                        workspace.join(format!(".exomonad/state/memory/{name}.md"))
                    )
                    .unwrap()
                )
            })
            .join("\n\n");
        assert_eq!(result.sections[0].body, original);
        assert_eq!(result.sections[0].source.as_deref(), Some("Persona.Orientation.startup; .exomonad/state/memory/{persona,human,curiosities}.md"));
        assert!(!workspace.join(".exomonad/state/notes").exists());
        assert!(matches!(
            judgments.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        host.input("What do you already remember about me?")
            .await
            .unwrap();
        let root = harness::model::AgentPath("/root".into());
        let mut pending = VecDeque::new();
        let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        super::startup_context_acceptance_tests::assert_request_user_texts(
            &host,
            host.context.actor.identity(),
            &first.request,
            &[
                result.rendered.as_str(),
                "What do you already remember about me?",
            ],
        )
        .await;
        let instructions = first.request.instructions.clone();
        let operation = first.operation("persona-refresh");
        first.sync_call_named(
            "persona-refresh",
            "haskell",
            include_str!("fixtures/startup_persona_refresh.hs"),
        );
        let refreshed = host
            .while_operation_succeeds(
                &operation,
                next_hosted_script_round(&mut requests, &mut pending, &root),
            )
            .await
            .unwrap();
        assert_eq!(
            committed_display_text(&refreshed.settled_output("persona-refresh")),
            "True"
        );
        let updated = ["persona", "human", "curiosities"]
            .map(|name| {
                format!(
                    "[Memory file: .exomonad/state/memory/{name}.md]\n{}",
                    std::fs::read_to_string(
                        workspace.join(format!(".exomonad/state/memory/{name}.md"))
                    )
                    .unwrap()
                )
            })
            .join("\n\n");
        super::startup_context_acceptance_tests::assert_request_user_texts(
            &host,
            host.context.actor.identity(),
            &refreshed.request,
            &[updated.as_str(), "What do you already remember about me?"],
        )
        .await;
        assert_eq!(refreshed.request.instructions, instructions);
        assert_eq!(
            host.context
                .forest
                .initial_context_observation(host.context.actor.identity())
                .await
                .unwrap(),
            retained
        );
        let refusal_operation = refreshed.operation("persona-native-refusal");
        refreshed.sync_call_named(
            "persona-native-refusal",
            "haskell",
            include_str!("fixtures/startup_persona_refuse_native.hs"),
        );
        let refused = host
            .while_operation_succeeds(
                &refusal_operation,
                next_hosted_script_round(&mut requests, &mut pending, &root),
            )
            .await
            .unwrap();
        assert_eq!(
            committed_display_text(&refused.settled_output("persona-native-refusal")),
            "True"
        );
        super::startup_context_acceptance_tests::assert_request_user_texts(
            &host,
            host.context.actor.identity(),
            &refused.request,
            &[updated.as_str(), "What do you already remember about me?"],
        )
        .await;
        assert_eq!(refused.request.instructions, instructions);
        assert!(matches!(
            judgments.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        refused.finish();
        settled_head(&host).await;
        for completed in [&operation, &refusal_operation] {
            assert!(matches!(
                host.runtime.scheduler().output(completed).await.unwrap(),
                Some(harness::turn::JobOutput::Completed(Ok(_)))
            ));
        }
    }
    let forest = Arc::downgrade(&host.context.forest);
    let stopped = host.stop().await;
    if stopped.is_err() {
        files.disable_cleanup(true);
        repository.disable_cleanup(true);
    }
    stopped.unwrap();
    assert_eq!(forest.strong_count(), 0);
    let records =
        exomonad_actor::ActorRecoveryJournal::read_observed(&run.join("actor-lifecycle.v2.jsonl"))
            .unwrap();
    let retired = super::latest_durable_root_application(&records)
        .unwrap()
        .unwrap();
    assert_eq!(
        retired.terminal.as_ref().unwrap().kind,
        exomonad_actor::ActorExitKind::Cancelled
    );
    let source = retired
        .application
        .as_ref()
        .unwrap()
        .accepted_source
        .as_deref();
    assert!(super::durable_root_identity(&records, source)
        .unwrap()
        .is_none());
    assert!(matches!(
        judgments.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn persona_web_selection_retains_native_judgment_and_empty_control() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut judgments) = mpsc::unbounded_channel();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        configure_persona(config, "Persona.Spec.agentSpec");
        config.jev = Some(Arc::new(PersonaJevGate(entered)));
    })
    .await
    .unwrap();
    host.run_scenario(|host| Box::pin(async move {
        host.input("Explore these passages").await.unwrap();
        let mut pending = VecDeque::new();
        let root = harness::model::AgentPath("/root".into());
        let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let operation = first.operation("persona-web-selection");
            first.sync_call_named(
                "persona-web-selection",
                "haskell",
                include_str!("fixtures/startup_persona_web_selection.hs"),
            );
            let judgment = host
                .while_operation_succeeds(
                    &operation,
                    tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, judgments.recv()),
                )
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(judgment.packet["state"]["intent"], "Retain both passages");
            assert_eq!(judgment.packet["state"]["conversation"], "[explicit intent only]");
            let questions = judgment.packet["questions"].as_object().unwrap();
            assert_eq!(questions.len(), 2);
            let answers = questions
                .iter()
                .map(|(key, question)| {
                    assert_eq!(question["type"], "noul");
                    (key.clone(), json!({"type":"noul", "noul":1.0}))
                })
                .collect::<serde_json::Map<_, _>>();
            let packet = judgment.packet.clone();
            judgment.answer.send(json!({
                "model":"controlled-web", "answers":answers, "usage":{}
            })).unwrap();
            let root = harness::model::AgentPath("/root".into());
            let selected = host.while_operation_succeeds(&operation, async {
                tokio::select! {
                    round = next_hosted_script_round(&mut requests, &mut pending, &root) => round,
                    extra = judgments.recv() => panic!("empty page issued another semantic observation: {:?}", extra.map(|judgment| judgment.packet)),
                }
            }).await.unwrap();
            assert_eq!(committed_display_text(&selected.settled_output("persona-web-selection")), "True");
            let saved = preparation(&host.context.config.workspace, "web-evaluation");
            assert_eq!(saved["outcome"], "completed");
            assert_eq!(saved["observations"].as_array().unwrap().len(), 1);
            assert_eq!(saved["observations"][0]["request"], packet);
            assert_eq!(saved["observations"][0]["occurrences"].as_array().unwrap().len(), 2);
            assert_eq!(preparation(&host.context.config.workspace, "empty-web-evaluation"),
                json!({"outcome":"completed", "observations":[]}));
            selected.finish();
    })).await;
}

fn preparation(workspace: &Path, name: &str) -> Value {
    let text = std::fs::read_to_string(workspace.join(format!(".exomonad/state/notes/{name}.md")))
        .unwrap();
    serde_json::from_str(&text).expect("persona web evaluation uses native JSON")
}
