//! The delivered persona carries full authored memory to the first provider
//! request and refreshes that exact issued block only on explicit activation.

use super::hosted_test_context::{HostedTestRuntime, ScenarioOutcome};
use super::test_campaign::{
    commit_workspace, hosted_script_provider, hosted_test_settings, next_hosted_script_round,
    retained_committed_display_text, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
};
use exomonad_actor::{InitialContextState, JevCallFailure};
use futures_util::FutureExt;
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

const PERSONA_MEMORIES: [(&str, &str); 3] = [
    ("persona", "persona-memory-original"),
    (
        "human",
        "A saved correction: answer from supplied memory when asked what you already remember.",
    ),
    ("curiosities", "curiosity-memory-original"),
];

const PERSONA_PROMPTS: [(&str, &str); 6] = [
    ("base.md", include_str!("../../../../exomonad/examples/persona/.exomonad/prompts/base.md")),
    ("agent.md", include_str!("../../../../exomonad/examples/persona/.exomonad/prompts/agent.md")),
    ("api-guide.md", include_str!("../../../../exomonad/examples/persona/.exomonad/prompts/api-guide.md")),
    ("references/companion-compositions.md", include_str!("../../../../exomonad/examples/persona/.exomonad/prompts/references/companion-compositions.md")),
    ("references/typesafe-compositions.md", include_str!("../../../../exomonad/examples/persona/.exomonad/prompts/references/typesafe-compositions.md")),
    ("typesafe-ai-LICENSE.md", include_str!("../../../../exomonad/examples/persona/.exomonad/prompts/typesafe-ai-LICENSE.md")),
];

fn retained_location(config: &mut super::ActorHostConfig, workspace: &Path, run: &Path) {
    config.workspace = workspace.to_owned();
    config.run_directory =
        tidepool_atomic_write::DirectoryAnchor::open_existing(run.parent().unwrap())
            .unwrap()
            .child(run.file_name().unwrap())
            .unwrap();
    config.root_binding_path = run.join("root-binding.json");
}

/// Author the complete fixture before its single immutable production selection.
pub(super) fn configure_persona(
    config: &mut super::ActorHostConfig,
    entry: &str,
    author: impl FnOnce(&Path, &mut crate::exomonad::ExomonadConfig),
) {
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Persona")).unwrap();
    std::fs::create_dir_all(authored.join("Jev")).unwrap();
    let sources = [
        (
            "Persona/FileTools.hs",
            tidepool_testing::fixture_source(
                "exomonad/examples/persona/.exomonad/Persona/FileTools.hs",
            ),
        ),
        (
            "Persona/Patch.hs",
            tidepool_testing::fixture_source(
                "exomonad/examples/persona/.exomonad/Persona/Patch.hs",
            ),
        ),
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
    for (relative, source) in PERSONA_PROMPTS {
        let destination = authored.join("prompts").join(relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, source).unwrap();
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
        project.launch.embedded = config.embedded.clone();
        project.prompts.base = Some("prompts/base.md".into());
        project.prompts.agent = Some("prompts/agent.md".into());
        project.prompts.api_guide = Some("prompts/api-guide.md".into());
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec![
            "Persona.FileTools".into(),
            "Persona.Patch".into(),
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
        author(&config.workspace, project);
    });
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .unwrap(),
    );
    crate::exomonad::source::SourceLayer::new(config.run_directory.path())
        .ensure_active(config.workspace_inputs.as_ref().unwrap())
        .unwrap();
    let memory = authored.join("state/memory");
    std::fs::create_dir_all(&memory).unwrap();
    for (name, body) in PERSONA_MEMORIES {
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
        configure_persona(config, "AgentSpec.agentSpec", |_, _| {});
        config.jev = Some(Arc::new(PersonaJevGate(entered)));
    })
    .await
    .unwrap();
    let forest = Arc::downgrade(&host.context.forest);
    let scenario = std::panic::AssertUnwindSafe(async {
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
        let original = PERSONA_MEMORIES
            .map(|(name, expected)| {
                assert_eq!(
                    std::fs::read_to_string(
                        workspace.join(format!(".exomonad/state/memory/{name}.md"))
                    )
                    .unwrap(),
                    expected
                );
                format!("[Memory file: .exomonad/state/memory/{name}.md]\n{expected}")
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
        let frozen = host.context.config.workspace_inputs.as_ref().unwrap();
        for (key, relative) in [
            ("base", "base.md"),
            ("agent", "agent.md"),
            ("api-guide", "api-guide.md"),
        ] {
            assert_eq!(
                frozen.prompts[key],
                PERSONA_PROMPTS
                    .iter()
                    .find(|(path, _)| *path == relative)
                    .unwrap()
                    .1
            );
        }
        let authored_prefix = format!(
            "{}\n\n{}\n\n{}",
            PERSONA_PROMPTS[0].1, PERSONA_PROMPTS[2].1, PERSONA_PROMPTS[1].1
        );
        let provider_request = serde_json::to_value(&first.request).unwrap();
        assert!(provider_request["instructions"].as_str().unwrap().starts_with(&authored_prefix), "first provider instructions must carry the frozen authored base, guide and companion profile");
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
            retained_committed_display_text(
                &host.runtime.store(),
                &harness::store::actor_output::ActorOutputOrigin {
                    run: host.runtime.run_identity().to_owned(),
                    native_actor: host.context.actor.identity().id.0,
                    incarnation: host.context.actor.identity().incarnation.0,
                },
                &refreshed.settled_output("persona-refresh"),
            ),
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
            retained_committed_display_text(
                &host.runtime.store(),
                &harness::store::actor_output::ActorOutputOrigin {
                    run: host.runtime.run_identity().to_owned(),
                    native_actor: host.context.actor.identity().id.0,
                    incarnation: host.context.actor.identity().incarnation.0,
                },
                &refused.settled_output("persona-native-refusal"),
            ),
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
    let target = {
        let conversation = host
            .context
            .binding(host.context.actor.identity())
            .unwrap()
            .conversation()
            .unwrap();
        conversation.identity().clone()
    };
    let retirement = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
    host.http_command(
        retirement,
        harness::server::HostCommand::Retire {
            target: target.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        host.command_receipt(&target.run, retirement).await.unwrap(),
        harness::server::CommandReceiptOutcome::ControlRequested {
            target,
            control: harness::server::CommandControl::Retire,
        }
    );
    assert_eq!(
        host.context.actor.terminal().get().unwrap().kind,
        exomonad_actor::ActorExitKind::Cancelled
    );
    }).catch_unwind().await;
    let scenario_outcome = ScenarioOutcome::from_result(&scenario);
    let stopped = host.stop(&scenario_outcome).await;
    if scenario.is_err() || stopped.is_err() {
        files.disable_cleanup(true);
        repository.disable_cleanup(true);
    }
    if let Err(payload) = scenario {
        if let Err(cleanup) = stopped {
            eprintln!("persona cleanup failed after scenario assertion: {cleanup}");
        }
        std::panic::resume_unwind(payload);
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
async fn persona_with_shared_workspace_keeps_effects_and_later_cells_usable() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        super::test_campaign::scaffold_workspace(&config.workspace);
        configure_persona(config, "AgentSpec.agentSpec", |workspace, project| {
            project.haskell.source_roots = vec![".".into(), "workspace".into()];
            project
                .haskell
                .modules
                .retain(|module| module != "Jev.Operators");
            std::fs::remove_file(workspace.join(".exomonad/Jev/Operators.hs")).unwrap();
            std::fs::write(
                workspace.join(".exomonad/AgentSpec.hs"),
                include_str!("fixtures/persona_shared_jev_agent_spec.hs"),
            )
            .unwrap();
            std::fs::write(
                workspace.join(".exomonad/Persona/ReentryProbe.hs"),
                include_str!("fixtures/persona_shared_jev_reentry_probe.hs"),
            )
            .unwrap();
        });
        std::fs::create_dir_all(config.workspace.join(".exomonad/state/notes")).unwrap();
        std::fs::write(
            config
                .workspace
                .join(".exomonad/state/notes/reentry-probe.json"),
            r#"{"observation":"ready"}"#,
        )
        .unwrap();
    })
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Inspect the workspace and its callable tools")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = VecDeque::new();
            let mut round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let origin = harness::store::actor_output::ActorOutputOrigin {
                run: host.runtime.run_identity().to_owned(),
                native_actor: host.context.actor.identity().id.0,
                incarnation: host.context.actor.identity().incarnation.0,
            };

            let first = round.operation("persona-memory-and-notes");
            round.sync_call_named(
                "persona-memory-and-notes",
                "haskell",
                "import qualified Tidepool.Effects.Authored as Files\n\
             import qualified Persona.Memory as Memory\n\
             notes <- Files.listDirectory \".exomonad/state/notes\"\n\
             display notes\n\
             curiosities <- Memory.recall Memory.Curiosities\n\
             display curiosities",
            );
            round = host
                .while_operation_succeeds(
                    &first,
                    next_hosted_script_round(&mut requests, &mut pending, &root),
                )
                .await
                .unwrap();
            let pages = super::test_campaign::retained_committed_display_texts(
                &host.runtime.store(),
                &origin,
                &round.settled_output("persona-memory-and-notes"),
            );
            assert_eq!(
                pages.len(),
                2,
                "first cell must publish both effects: {pages:?}"
            );
            assert!(pages[0].starts_with("Right "), "{pages:?}");
            assert!(pages[1].contains("curiosity-memory-original"), "{pages:?}");

            let pure = round.operation("persona-pure-after-effects");
            round.sync_call_named(
                "persona-pure-after-effects",
                "haskell",
                "display (1 + 1 :: Int)",
            );
            round = host
                .while_operation_succeeds(
                    &pure,
                    next_hosted_script_round(&mut requests, &mut pending, &root),
                )
                .await
                .unwrap();
            assert_eq!(
                retained_committed_display_text(
                    &host.runtime.store(),
                    &origin,
                    &round.settled_output("persona-pure-after-effects"),
                ),
                "2"
            );
            round.finish();
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn authored_persona_directory_reads_leave_later_cells_usable() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        configure_persona(config, "AgentSpec.agentSpec", |workspace, _| {
            let notes = workspace.join(".exomonad/state/notes");
            std::fs::create_dir_all(&notes).unwrap();
            std::fs::write(notes.join("history-seed.md"), "retained note\n").unwrap();
        });
    })
    .await
    .unwrap();
    host.run_scenario(|host| Box::pin(async move {
        let startup = host.context.forest
            .initial_context_observation(host.context.actor.identity())
            .await.unwrap();
        assert_eq!(startup.state, InitialContextState::Published);
        let seed = startup.result.as_ref().unwrap();
        assert_eq!(seed.sections.len(), 1);
        for (_, memory) in PERSONA_MEMORIES {
            assert!(seed.sections[0].body.contains(memory), "{}", seed.sections[0].body);
        }

        host.input("Read the notes and memory directories").await.unwrap();
        let mut pending = VecDeque::new();
        let root = harness::model::AgentPath("/root".into());
        let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        super::startup_context_acceptance_tests::assert_request_user_texts(
            host, host.context.actor.identity(), &first.request,
            &[seed.rendered.as_str(), "Read the notes and memory directories"],
        ).await;
        let notes_operation = first.operation("persona-notes-list");
        let memory_operation = first.operation("persona-memory-list");
        first.sync_calls_named(&[
            ("persona-notes-list", "haskell", "import qualified Tidepool.Effects.Authored as Files\nlisting <- Files.listDirectory \".exomonad/state/notes\"\ndisplay listing"),
            ("persona-memory-list", "haskell", "import qualified Tidepool.Effects.Authored as Files\nmem <- Files.listDirectory \".exomonad/state/memory\"\ndisplay mem"),
        ]);
        let after_reads = host.while_operation_succeeds(
            &notes_operation,
            host.while_operation_succeeds(
                &memory_operation,
                next_hosted_script_round(&mut requests, &mut pending, &root),
            ),
        ).await.unwrap().unwrap();
        let origin = harness::store::actor_output::ActorOutputOrigin {
            run: host.runtime.run_identity().to_owned(),
            native_actor: host.context.actor.identity().id.0,
            incarnation: host.context.actor.identity().incarnation.0,
        };
        let notes = retained_committed_display_text(
            &host.runtime.store(), &origin, &after_reads.settled_output("persona-notes-list"));
        assert!(notes.starts_with("Right ") && notes.contains("history-seed.md"), "{notes}");
        let memory = retained_committed_display_text(
            &host.runtime.store(), &origin, &after_reads.settled_output("persona-memory-list"));
        assert!(memory.starts_with("Right ") && memory.contains("persona.md")
            && memory.contains("human.md") && memory.contains("curiosities.md"), "{memory}");

        let reuse_operation = after_reads.operation("persona-later-reuse");
        after_reads.sync_call_named("persona-later-reuse", "haskell",
            "display listing\ndisplay mem\ndisplay (1 + 1 :: Int)");
        let after_reuse = host.while_operation_succeeds(
            &reuse_operation,
            next_hosted_script_round(&mut requests, &mut pending, &root),
        ).await.unwrap();
        assert_eq!(super::test_campaign::retained_committed_display_texts(
            &host.runtime.store(), &origin, &after_reuse.settled_output("persona-later-reuse")),
            vec![notes, memory, "2".to_owned()]);

        let installation = host.context.observer.installation(host.context.actor.identity()).await;
        after_reuse.finish();
    })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn persona_snapshot_memory_examples_preserve_versions_errors_and_context() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        configure_persona(config, "AgentSpec.agentSpec", |_, _| {});
    })
    .await
    .unwrap();
    host.run_scenario(|host| Box::pin(async move {
        host.input("Try a version-checked memory edit").await.unwrap();
        let mut pending = VecDeque::new();
        let root = harness::model::AgentPath("/root".into());
        let mut round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        let memory_source = include_str!("fixtures/api-guide/persona-memory-edit.hs");
        assert!(round.request.instructions.contains(memory_source), "the published memory example must match the executed source");
        let origin = harness::store::actor_output::ActorOutputOrigin {
            run: host.runtime.run_identity().to_owned(),
            native_actor: host.context.actor.identity().id.0,
            incarnation: host.context.actor.identity().incarnation.0,
        };
        for (call, source, expected) in [
            ("snapshot-context-before", "import qualified Tidepool.Agent.Context as C\nimport Control.Lens (view)\nC.InitialContextPublished _ _ _ initialMemoryReference <- C.getInitialContext\nbeforeMemoryContext <- C.getContext\nlet memoryBlockTexts context = [body | C.Text (Just reference) _ body _ <- view C.contextBlocks context, reference == initialMemoryReference]\nlet originalMemoryBlock = memoryBlockTexts beforeMemoryContext\n", None),
            ("persona-memory-edit", memory_source, Some("Written")),
            ("file-snapshot-example", include_str!("../../../../exomonad/examples/workspace/.exomonad/checks/file-snapshot-example.hs"), Some("True")),
            ("snapshot-refusals", include_str!("fixtures/persona_snapshot_refusals.hs"), Some("True")),
        ] {
            let operation = round.operation(call);
            round.sync_call_named(call, "haskell", source);
            round = host.while_operation_succeeds(&operation,
                next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
            if let Some(expected) = expected {
                let pages = super::test_campaign::retained_committed_display_texts(&host.runtime.store(), &origin, &round.settled_output(call));
                assert_eq!(pages.len(), if call == "snapshot-refusals" { 1 } else { 2 }, "{call}: {pages:?}");
                let text = pages.join("\n");
                assert!(text.contains(expected), "{call}: {text}");
                if call == "file-snapshot-example" {
                    assert!(text.contains("FileSnapshot") && text.contains("ContentVersion"), "snapshot display: {text}");
                }
            }
        }
        for (call, source, expected) in [
            ("snapshot-private-constructor", "let forged = File.FileSnapshot\n", "File.FileSnapshot"),
            ("version-private-constructor", "let forged = File.ContentVersion\n", "File.ContentVersion"),
            ("snapshot-no-independent-path", "File.writeSnapshot target \"wrong destination\"\n", "FileSnapshot"),
        ] {
            let operation = round.operation(call);
            round.sync_call_named(call, "haskell", source);
            round = host.while_operation_succeeds(&operation,
                next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
            super::test_campaign::require_ghc_compile_rejection(&round.settled_output(call), &[expected]).unwrap();
        }
        let workspace = &host.context.config.workspace;
        assert_eq!(std::fs::read_to_string(workspace.join(".exomonad/state/memory/human.md")).unwrap(),
            format!("{}\nAn experiment worth returning to.\n", PERSONA_MEMORIES[1].1));
        assert!(!workspace.join(".exomonad/state/notes/missing-snapshot.md").exists());
        assert!(!workspace.join(".exomonad/state/notes/missing-revision.md").exists());
        round.finish();
    })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn persona_web_selection_retains_native_judgment_and_empty_control() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut judgments) = mpsc::unbounded_channel();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        configure_persona(config, "Persona.Spec.agentSpec", |_, _| {});
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
                    tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                        loop {
                            tokio::select! {
                                biased;
                                judgment = judgments.recv() => break judgment,
                                round = next_hosted_script_round(&mut requests, &mut pending, &root) => {
                                    let returned = round.request.input.iter().filter(|item|
                                        item.0["type"] == "custom_tool_call_output"
                                            && item.0["call_id"] == operation.call.0
                                    ).collect::<Vec<_>>();
                                    if !returned.is_empty() {
                                        let recorded = host.runtime.store()
                                            .replay_tool_output_operation(&operation).unwrap()
                                            .expect("the exact pending operation must own its returned output");
                                        let projected = super::embedded_policy::project_retained_result(&recorded, &operation)
                                            .expect("returned native output must authenticate its original operation")
                                            .expect("the exact native output must have a model presentation");
                                        let expected = harness::item::Item::tool_output(
                                            &operation.call, harness::item::ToolKind::Custom,
                                            &harness::turn::JobOutput::Completed(Ok(projected)),
                                        );
                                        assert_eq!(returned, vec![&expected],
                                            "the observed call output must be the exact pending operation's presentation");
                                        panic!("persona web selection settled before its judgment: {operation:?}: {:?}: {}",
                                            recorded.terminal, round.settled_output(&operation.call.0));
                                    }
                                    round.finish();
                                }
                            }
                        }
                    }),
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
            assert_eq!(retained_committed_display_text(
                &host.runtime.store(),
                &harness::store::actor_output::ActorOutputOrigin {
                    run: host.runtime.run_identity().to_owned(),
                    native_actor: host.context.actor.identity().id.0,
                    incarnation: host.context.actor.identity().incarnation.0,
                },
                &selected.settled_output("persona-web-selection"),
            ), "True");
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

#[tokio::test(flavor = "multi_thread")]
async fn authored_runtime_info_reports_preparing_then_actual_publication() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        configure_persona(config, "Persona.Spec.runtimeSpec", |_, _| {});
    })
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            // The spec is polymorphic; completed preparation owns the exact
            // admitted specialization, independently of its illustrative aliases.
            let coverage = host.context.config.workspace_inputs.as_ref().unwrap()
                .prepared_toolset_coverage().unwrap();
            assert_eq!(coverage.len(), 1);
            let base_effects = coverage[0].effective_effects.iter().map(|key| {
                format!("ActorEffect {}", key.haskell_constructor().unwrap())
            }).collect::<Vec<_>>();
            let original_generation = host.context.observer
                .installation(host.context.actor.identity()).await.policy
                .snapshot_for_request().unwrap().request_generation().unwrap().get();
            let retained = host
                .context
                .forest
                .initial_context_observation(host.context.actor.identity())
                .await
                .unwrap();
            assert_eq!(retained.state, InitialContextState::Published);
            let seed = retained.result.as_ref().unwrap();
            assert_eq!(seed.sections.len(), 3);
            assert_eq!(seed.sections[2].label, "effects at arrival");
            let docs_at_arrival = &seed.sections[2].body;
            assert!(docs_at_arrival.contains("EffectCommands"), "{docs_at_arrival}");
            assert!(docs_at_arrival.contains("EffectRuntimeInfo"), "{docs_at_arrival}");
            assert!(docs_at_arrival.contains("currentEffectDocs ::"), "{docs_at_arrival}");
            let documented_effects = docs_at_arrival.lines()
                .filter(|line| line.starts_with("ActorEffect "))
                .collect::<Vec<_>>();
            assert_eq!(documented_effects, base_effects, "{docs_at_arrival}");
            assert!(!docs_at_arrival.contains("InvocationContextReadWrite"), "{docs_at_arrival}");
            assert!(!docs_at_arrival.contains("InvocationReload"), "{docs_at_arrival}");
            assert!(docs_at_arrival.contains("Catalogue documentation unavailable."), "{docs_at_arrival}");
            assert_eq!(seed.sections[1].label, "runtime at arrival");
            let at_arrival = &seed.sections[1].body;
            assert!(
                at_arrival.contains("Startup as of this observation: preparing"),
                "{at_arrival}"
            );
            assert!(
                !at_arrival.contains("context published"),
                "{at_arrival}"
            );
            assert!(
                at_arrival.contains("Persona.Spec.runtimeSpec"),
                "{at_arrival}"
            );
            assert!(
                at_arrival.contains("Configured inference:"),
                "{at_arrival}"
            );
            assert!(!at_arrival.contains("Configured inference: unavailable"), "{at_arrival}");
            assert!(
                at_arrival.contains("haskell (before next inference)"),
                "{at_arrival}"
            );
            host.input("Observe the completed startup").await.unwrap();
            let mut pending = VecDeque::new();
            let root = harness::model::AgentPath("/root".into());
            let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            super::startup_context_acceptance_tests::assert_request_user_texts(
                host,
                host.context.actor.identity(),
                &first.request,
                &[seed.rendered.as_str(), "Observe the completed startup"],
            )
            .await;
            let operation = first.operation("runtime-after-publication");
            let original_identity = retained.identity.as_ref().unwrap();
            // Observe typed facts before display: the full tool inventory can
            // exceed the ordinary page budget before startup evidence appears.
            let publication_source = include_str!("fixtures/startup_runtime_published.hs")
                .replace("EXPECTED_INSTALL", &original_identity.spec_install.to_string())
                .replace("EXPECTED_SOURCE", &format!("{:?}", original_identity.spec_source))
                .replace("EXPECTED_GENERATION", &original_generation.to_string())
                .replace("EXPECTED_EFFECTS", &format!(
                    "[InvocationContextReadWrite, InvocationReload, {}]", base_effects.join(", ")
                ));
            first.sync_call_named(
                "runtime-after-publication",
                "haskell",
                &publication_source,
            );
            let after = host
                .while_operation_succeeds(
                    &operation,
                    next_hosted_script_round(&mut requests, &mut pending, &root),
                )
                .await
                .unwrap();
            let text = retained_committed_display_text(
                &host.runtime.store(),
                &harness::store::actor_output::ActorOutputOrigin {
                    run: host.runtime.run_identity().to_owned(),
                    native_actor: host.context.actor.identity().id.0,
                    incarnation: host.context.actor.identity().incarnation.0,
                },
                &after.settled_output("runtime-after-publication"),
            );
            assert_eq!(text, "True", "expected published identity {original_identity:?} at generation {original_generation}");
            // Change actual selected source, then publish through the existing Reload owner.
            let orientation = host.context.config.workspace.join(".exomonad/Persona/Orientation.hs");
            let original_source = std::fs::read_to_string(&orientation).unwrap();
            std::fs::write(&orientation, format!("-- runtime-info reload observation\n{original_source}")).unwrap();
            let operation = after.operation("runtime-during-reload");
            after.sync_call_named("runtime-during-reload", "haskell",
                "Right runtimeReload <- reloadSource []\nRight duringReload <- getRuntimeInfo\nRight docsDuringReload <- currentEffectDocs\ndisplay (runtimeExecutingSpec duringReload == runtimeExecutingSpec runtimeFacts && runtimeExecutingGeneration duringReload == runtimeExecutingGeneration runtimeFacts && docsDuringReload == runtimeDocs)\n");
            let reloaded = host.while_operation_succeeds(&operation,
                next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
            let origin = harness::store::actor_output::ActorOutputOrigin {
                run: host.runtime.run_identity().to_owned(),
                native_actor: host.context.actor.identity().id.0,
                incarnation: host.context.actor.identity().incarnation.0,
            };
            assert_eq!(retained_committed_display_text(&host.runtime.store(), &origin,
                &reloaded.settled_output("runtime-during-reload")), "True");
            let operation = reloaded.operation("runtime-after-reload");
            let original_identity = retained.identity.as_ref().unwrap();
            // The post-reload notebook is fresh. Only owner-issued scalar facts
            // cross this boundary; no old notebook binding is required.
            let after_reload_source = format!(
                "import qualified Tidepool.Agent.Context as C\nRight afterReloadFacts <- getRuntimeInfo\ndisplay (case (runtimeExecutingSpec afterReloadFacts, runtimeStartup afterReloadFacts) of {{ (Just selected, RuntimeStartupObserved (C.InitialContextPublished initial _ _ _)) -> runtimeSpecInstall selected > {install} && runtimeSpecSource selected /= {source:?} && runtimeExecutingGeneration afterReloadFacts > Just {generation} && C.initialContextSpecInstall initial == {install} && C.initialContextSpecSource initial == {source:?}; _ -> False }})\n",
                install = original_identity.spec_install,
                source = original_identity.spec_source,
                generation = original_generation,
            );
            reloaded.sync_call_named("runtime-after-reload", "haskell", &after_reload_source);
            let final_round = host.while_operation_succeeds(&operation,
                next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
            assert_eq!(retained_committed_display_text(&host.runtime.store(), &origin,
                &final_round.settled_output("runtime-after-reload")), "True");
            final_round.finish();
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_info_distinguishes_absent_initializer_from_unobserved_startup() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        configure_persona(config, "Persona.Spec.helperSpec", |_, _| {});
    })
    .await
    .unwrap();
    host.run_scenario(|host| Box::pin(async move {
        assert_eq!(host.context.forest.initial_context_observation(host.context.actor.identity())
            .await.unwrap().state, InitialContextState::AbsentHook);
        assert_eq!(host.context.config.model, exomonad_actor::Model::for_backend(
            exomonad_actor::Backend::OpenAIPlan, "test-model"));
        assert_eq!(host.context.config.reasoning, exomonad_actor::Reasoning::OpenAIReasoning(
            exomonad_actor::OpenAIReasoning::OpenAILow));
        host.input("Observe this spec without an initializer").await.unwrap();
        let mut pending = VecDeque::new();
        let root = harness::model::AgentPath("/root".into());
        let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        let operation = first.operation("runtime-absent-initializer");
        first.sync_call_named("runtime-absent-initializer", "haskell",
            "import qualified Tidepool.Agent.Context as C\nRight observedRuntime <- getRuntimeInfo\ndisplay ((case runtimeStartup observedRuntime of { RuntimeStartupObserved C.InitialContextAbsent -> True; _ -> False }) && runtimeConfiguredInference observedRuntime == RuntimeConfiguredInference (C.CustomModel C.OpenAIPlan \"test-model\") (C.OpenAIReasoning C.OpenAILow))\n");
        let after = host.while_operation_succeeds(&operation,
            next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
        assert_eq!(retained_committed_display_text(&host.runtime.store(),
            &harness::store::actor_output::ActorOutputOrigin {
                run: host.runtime.run_identity().to_owned(),
                native_actor: host.context.actor.identity().id.0,
                incarnation: host.context.actor.identity().incarnation.0,
            }, &after.settled_output("runtime-absent-initializer")), "True");
        after.finish();
    })).await;
}
