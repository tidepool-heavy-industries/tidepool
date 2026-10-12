//! The delivered persona consumer uses the issued startup block, native Jev
//! decoding, native JSON state receipts, and explicit synchronous activation.

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

fn original_choices(packet: &Value) -> Value {
    let questions = packet["questions"].as_object().expect("native Jev packet");
    assert_eq!(
        questions.len(),
        4,
        "the parsed manifest adds a fourth memory"
    );
    let answers = questions
        .iter()
        .map(|(key, question)| {
            assert_eq!(question["type"], "choice");
            let criteria = question["criteria"].as_object().unwrap();
            assert!(criteria.contains_key("original"));
            let probabilities = criteria
                .keys()
                .map(|choice| {
                    (
                        choice.clone(),
                        json!(if choice == "original" { 1.0 } else { 0.0 }),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            (
                key.clone(),
                json!({"type":"choice", "choice":"original",
            "confidence":1.0, "probabilities": probabilities}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    json!({"model":"controlled-persona", "answers":answers, "usage":{}})
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
            "Persona/Spec.hs",
            tidepool_testing::fixture_source("exomonad/examples/persona/.exomonad/Persona/Spec.hs"),
        ),
        (
            "Persona/Attention.hs",
            tidepool_testing::fixture_source(
                "exomonad/examples/persona/.exomonad/Persona/Attention.hs",
            ),
        ),
        (
            "Persona/Collection.hs",
            tidepool_testing::fixture_source(
                "exomonad/examples/persona/.exomonad/Persona/Collection.hs",
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
    std::fs::write(
        authored.join("AgentSpec.hs"),
        include_str!("fixtures/startup_persona_spec.hs"),
    )
    .unwrap();
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
            "AgentSpec".into(),
            "Persona.Memory".into(),
            "Persona.Spec".into(),
            "Persona.Attention".into(),
            "Persona.Collection".into(),
            "Persona.Context".into(),
            "Jev.Operators".into(),
        ];
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
        ("human", "human-memory-original"),
        ("curiosities", "curiosity-memory-original"),
        ("extra", "extra-memory-original"),
    ] {
        std::fs::write(memory.join(format!("{name}.md")), body).unwrap();
    }
    std::fs::write(
        memory.join("collection.json"),
        json!({"version":1,"memories":[{
            "id":"extra", "path":"memory/extra.md", "tags":["extra"],
            "related":["persona"], "availableAt":10
        }]})
        .to_string(),
    )
    .unwrap();
}

fn user_texts(request: &harness::transport::ResponsesRequest) -> Vec<&str> {
    request
        .input
        .iter()
        .filter(|item| item.0["type"] == "message" && item.0["role"] == "user")
        .map(|item| item.0["content"].as_str().unwrap())
        .collect()
}

fn preparation(workspace: &Path, name: &str) -> Value {
    let text = std::fs::read_to_string(workspace.join(format!(".exomonad/state/notes/{name}.md")))
        .unwrap();
    serde_json::from_str(&text).expect("the actual persona save uses native JSON")
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
async fn actual_persona_prepares_saves_activates_and_resumes_edited_startup_context() {
    let files = tempfile::tempdir().unwrap();
    let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("README.md", "source\n", "seed")
        .unwrap();
    let workspace = repository.path().to_owned();
    let run = files.path().join("persona-run");
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut judgments) = mpsc::unbounded_channel();
    let backend = Arc::new(PersonaJevGate(entered));
    let configured_backend = backend.clone();
    let mut starting = Box::pin(HostedTestRuntime::start_configured(
        &settings,
        &provider,
        |config| {
            retained_location(config, &workspace, &run);
            configure_persona(config, "AgentSpec.agentSpec");
            config.jev = Some(configured_backend);
        },
    ));
    let judgment = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        tokio::select! {
            judgment = judgments.recv() => judgment.expect("persona startup asks native Jev"),
            started = &mut starting => match started {
                Ok(_) => panic!("persona startup bypassed the actual preparation"),
                Err(failure) => panic!("persona preparation failed before judgment: {failure}"),
            },
        }
    })
    .await
    .expect("persona startup reaches controlled judgment");
    let response = original_choices(&judgment.packet);
    judgment.answer.send(response).unwrap();
    let host = starting.await.unwrap();
    {
        let retained = host
            .context
            .forest
            .initial_context_observation(host.context.actor.identity())
            .unwrap();
        assert_eq!(retained.state, InitialContextState::Published);
        let saved = preparation(&workspace, "startup-preparation");
        assert_eq!(saved["passesUsed"], 1);
        assert_eq!(saved["retainedMemories"].as_array().unwrap().len(), 4);
        assert_eq!(
            saved["retainedMemories"][0]["original"],
            "persona-memory-original"
        );
        assert_eq!(
            saved["retainedMemories"][3]["original"],
            "extra-memory-original"
        );
        let result = retained.result.as_ref().unwrap();
        assert_eq!(
            result.sections[0].body,
            saved["contextText"].as_str().unwrap()
        );
        host.input("persona-activation").await.unwrap();
        let root = harness::model::AgentPath("/root".into());
        let mut pending = VecDeque::new();
        let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        assert_eq!(
            user_texts(&first.request),
            [result.rendered.as_str(), "persona-activation"]
        );
        let instructions = first.request.instructions.clone();
        let operation = first.operation("persona-refresh");
        first.sync_call_named(
            "persona-refresh",
            "haskell",
            include_str!("fixtures/startup_persona_refresh.hs"),
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
        let response = original_choices(&judgment.packet);
        judgment.answer.send(response).unwrap();
        let refreshed = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        assert_eq!(
            committed_display_text(&refreshed.settled_output("persona-refresh")),
            "True"
        );
        let updated = preparation(&workspace, "refresh-preparation");
        assert_eq!(
            updated["retainedMemories"][0]["original"],
            "persona-memory-updated"
        );
        assert_eq!(
            user_texts(&refreshed.request),
            [
                updated["contextText"].as_str().unwrap(),
                "persona-activation"
            ]
        );
        assert_eq!(refreshed.request.instructions, instructions);
        assert_eq!(
            host.context
                .forest
                .initial_context_observation(host.context.actor.identity())
                .unwrap(),
            retained
        );
        refreshed.sync_call_named(
            "persona-native-refusal",
            "haskell",
            include_str!("fixtures/startup_persona_refuse_native.hs"),
        );
        let refused = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        assert_eq!(
            committed_display_text(&refused.settled_output("persona-native-refusal")),
            "True"
        );
        assert_eq!(
            user_texts(&refused.request),
            [
                updated["contextText"].as_str().unwrap(),
                "persona-activation"
            ]
        );
        assert_eq!(refused.request.instructions, instructions);
        assert!(matches!(
            judgments.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        refused.finish();
        settled_head(&host).await;
    }
    host.stop().await.unwrap();

    let startup_before =
        std::fs::read(workspace.join(".exomonad/state/notes/startup-preparation.md")).unwrap();
    let updated = preparation(&workspace, "refresh-preparation");
    std::fs::write(
        workspace.join(".exomonad/state/memory/persona.md"),
        "changed-after-stop",
    )
    .unwrap();
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        retained_location(config, &workspace, &run);
        config.jev = Some(backend);
        config.workspace_inputs = Some(
            crate::exomonad::workspace::FrozenWorkspace::load(
                &workspace,
                &config.run_directory.path(),
            )
            .unwrap(),
        );
    })
    .await
    .unwrap();
    {
        assert!(matches!(
            judgments.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(
            std::fs::read(workspace.join(".exomonad/state/notes/startup-preparation.md")).unwrap(),
            startup_before
        );
        host.input("persona-resume-activation").await.unwrap();
        let root = harness::model::AgentPath("/root".into());
        let mut pending = VecDeque::new();
        let resumed = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        assert_eq!(
            user_texts(&resumed.request),
            [
                updated["contextText"].as_str().unwrap(),
                "persona-activation",
                "persona-resume-activation"
            ]
        );
        assert!(matches!(
            judgments.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        resumed.finish();
        settled_head(&host).await;
    }
    host.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn shipped_persona_initializer_uses_actual_preparation_before_first_request() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut judgments) = mpsc::unbounded_channel();
    let mut starting = Box::pin(HostedTestRuntime::start_configured(
        &settings,
        &provider,
        move |config| {
            configure_persona(config, "Persona.Spec.agentSpec");
            config.jev = Some(Arc::new(PersonaJevGate(entered)));
        },
    ));
    let judgment = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        tokio::select! {
            judgment = judgments.recv() => judgment.expect("shipped persona prepares through native Jev"),
            started = &mut starting => match started {
                Ok(_) => panic!("shipped persona became ready before its judgment"),
                Err(failure) => panic!("shipped persona failed before judgment: {failure}"),
            },
        }
    }).await.unwrap();
    assert!(judgment.packet["state"]["occasion"]
        .as_str()
        .is_some_and(|occasion| !occasion.is_empty()));
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    judgment
        .answer
        .send(original_choices(&judgment.packet))
        .unwrap();
    let host = starting.await.unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            let retained = host
                .context
                .forest
                .initial_context_observation(host.context.actor.identity())
                .unwrap();
            assert_eq!(retained.state, InitialContextState::Published);
            let result = retained.result.as_ref().unwrap();
            assert_eq!(result.sections.len(), 1);
            assert_eq!(
                result.sections[0].source.as_deref(),
                Some("Persona.Context.prepare")
            );
            for (name, expected) in [
                ("persona", "persona-memory-original"),
                ("human", "human-memory-original"),
                ("curiosities", "curiosity-memory-original"),
                ("extra", "extra-memory-original"),
            ] {
                assert_eq!(
                    std::fs::read_to_string(
                        host.context
                            .config
                            .workspace
                            .join(format!(".exomonad/state/memory/{name}.md"))
                    )
                    .unwrap(),
                    expected
                );
                assert!(result.sections[0].body.contains(expected));
            }
            host.input("canonical-persona-activation").await.unwrap();
            let mut pending = VecDeque::new();
            let first = next_hosted_script_round(
                &mut requests,
                &mut pending,
                &harness::model::AgentPath("/root".into()),
            )
            .await;
            assert_eq!(
                user_texts(&first.request),
                [result.rendered.as_str(), "canonical-persona-activation"]
            );
            assert!(matches!(
                judgments.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            first.finish();
        })
    })
    .await;
}
