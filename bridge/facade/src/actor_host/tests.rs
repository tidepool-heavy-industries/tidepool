pub(super) use super::test_campaign::{
    dispatch_haskell_script, dispatch_haskell_script_result, dispatch_lookup, dispatch_status,
    dispatch_structured_tool,
};
use super::*;
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
use exomonad_worktree::WorktreeSpec;
use std::error::Error as _;

#[derive(Debug, PartialEq, Eq)]
struct HandoffTestError(&'static str);

impl std::fmt::Display for HandoffTestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for HandoffTestError {}

async fn finished_application_task() -> tokio::task::JoinHandle<Result<(), String>> {
    let task = tokio::spawn(async { Ok(()) });
    while !task.is_finished() {
        tokio::task::yield_now().await;
    }
    task
}

#[tokio::test]
async fn application_handoff_keeps_both_typed_failures_and_primary_source() {
    let owners = Arc::new(Mutex::new(HashMap::new()));
    assert!(handoff_application_owners(
        owners.clone(),
        finished_application_task().await,
        Ok(()),
        Ok(()),
    )
    .is_ok());

    let cleanup_only = handoff_application_owners(
        owners.clone(),
        finished_application_task().await,
        Err(Box::new(HandoffTestError("cleanup only"))),
        Ok(()),
    )
    .expect_err("cleanup failure is propagated");
    assert_eq!(
        cleanup_only.downcast_ref::<HandoffTestError>(),
        Some(&HandoffTestError("cleanup only"))
    );

    let run_only = handoff_application_owners(
        owners.clone(),
        finished_application_task().await,
        Ok(()),
        Err(Box::new(HandoffTestError("run only"))),
    )
    .expect_err("run failure is propagated");
    assert_eq!(
        run_only.downcast_ref::<HandoffTestError>(),
        Some(&HandoffTestError("run only"))
    );

    let both = handoff_application_owners(
        owners.clone(),
        finished_application_task().await,
        Err(Box::new(HandoffTestError("cleanup detail"))),
        Err(Box::new(HandoffTestError("run detail"))),
    )
    .expect_err("run failure remains visible when cleanup also fails");
    let pair = both
        .downcast::<ApplicationRunCleanupError>()
        .expect("both non-retained failures have an error-pair owner");
    assert_eq!(
        pair.run.downcast_ref::<HandoffTestError>(),
        Some(&HandoffTestError("run detail"))
    );
    assert_eq!(
        pair.cleanup.downcast_ref::<HandoffTestError>(),
        Some(&HandoffTestError("cleanup detail"))
    );
    assert_eq!(
        pair.source()
            .and_then(|source| source.downcast_ref::<HandoffTestError>()),
        Some(&HandoffTestError("run detail"))
    );
    assert_eq!(
        pair.to_string(),
        "application run failed: run detail; application cleanup failed: cleanup detail"
    );

    let unfinished =
        tokio::spawn(async { futures_util::future::pending::<Result<(), String>>().await });
    let both = handoff_application_owners(
        owners,
        unfinished,
        Err(Box::new(HandoffTestError("cleanup detail"))),
        Err(Box::new(HandoffTestError("run detail"))),
    )
    .expect_err("unfinished application resources retain the handoff error");
    let retained = both
        .downcast::<RetainedInteractiveFleet>()
        .expect("retained resources keep their carrier");
    assert_eq!(retained.failures.len(), 1);
    let pair = retained.failures[0]
        .downcast_ref::<ApplicationRunCleanupError>()
        .expect("both failures are represented by their owning error pair");
    assert_eq!(
        pair.run.downcast_ref::<HandoffTestError>(),
        Some(&HandoffTestError("run detail"))
    );
    assert_eq!(
        pair.cleanup.downcast_ref::<HandoffTestError>(),
        Some(&HandoffTestError("cleanup detail"))
    );
    assert_eq!(
        pair.source()
            .and_then(|source| source.downcast_ref::<HandoffTestError>()),
        Some(&HandoffTestError("run detail"))
    );
    assert_eq!(
        pair.to_string(),
        "application run failed: run detail; application cleanup failed: cleanup detail"
    );
    retained.unfinished.as_ref().unwrap().abort();
}

#[tokio::test]
async fn unbounded_repository_event_await_joins_before_actor_retirement() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let baseline = campaign
                    .forest
                    .measurement_snapshot()
                    .and_then(|snapshot| snapshot.parked)
                    .expect("bootstrapped resident measurement");
                let actor = campaign.actor.clone();
                let policy = campaign.root_installation.policy.clone();
                let invocation = tokio::spawn(async move {
                    policy
            .dispatch_json_boxed(ToolInvocation {
                context: None,
                name: exomonad_actor::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(
                    "do { sub <- send (RepoEventSubscribe [WatchDeadline 3600000]) >>= liftEither; \
                     awaitSubscriptionRaw sub (-1) >>= liftEither }"
                        .into(),
                ),
            })
            .await
                });

                tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        if campaign
                            .forest
                            .measurement_snapshot()
                            .and_then(|snapshot| snapshot.parked)
                            .is_some_and(|parked| parked > baseline)
                        {
                            break;
                        }
                        assert!(!invocation.is_finished(), "Event await never parked");
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("Event await did not park");

                tokio::time::timeout(
                    Duration::from_secs(30),
                    actor.shutdown(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "retire parked repository event await".into(),
                        diagnostic: None,
                    }),
                )
                .await
                .expect("actor retirement waited forever for Event await")
                .expect("actor shutdown request");
                tokio::time::timeout(Duration::from_secs(30), invocation)
                    .await
                    .expect("Event invocation did not settle after retirement")
                    .expect("Event invocation task panicked")
                    .expect_err("cancelled Event await must not complete successfully");
                tokio::time::timeout(
                    Duration::from_secs(30),
                    campaign.observe_hosted_completion(),
                )
                .await
                .expect("hosted actor did not settle")
                .expect("hosted actor task panicked");
                assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Cancelled);
                assert!(actor.terminal().cleanup().unwrap().is_confirmed());
                assert_eq!(
                    campaign
                        .forest
                        .measurement_snapshot()
                        .and_then(|snapshot| snapshot.parked),
                    Some(0),
                    "retirement left an Event continuation parked"
                );
            })
        })
        .await;
}

#[test]
fn recorded_run_root_selects_its_own_managed_worktree_family() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    let legacy_run = root.path().join("cache/exomonad/runs/old-run");
    let state_run = root.path().join("state/exomonad/runs/new-run");
    assert_eq!(
        actor_worktree_storage_root(&workspace, &legacy_run).unwrap(),
        actor_worktree_storage_root_in(&root.path().join("cache/exomonad"), &workspace)
    );
    assert_eq!(
        actor_worktree_storage_root(&workspace, &state_run).unwrap(),
        actor_worktree_storage_root_in(&root.path().join("state/exomonad"), &workspace)
    );
}

#[test]
fn empty_legacy_directories_do_not_block_but_retained_content_does() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("actor-worktrees/project");
    std::fs::create_dir_all(legacy.join("bindings")).unwrap();
    std::fs::write(legacy.join("bindings/.owner.lock"), "").unwrap();
    assert!(!legacy_has_meaningful_state(&legacy).unwrap());
    std::fs::write(legacy.join("bindings/worktree.json"), "retained").unwrap();
    assert!(legacy_has_meaningful_state(&legacy).unwrap());
}

#[test]
fn driver_sources_use_run_captured_libraries() {
    let project = tempfile::tempdir().unwrap();
    let run = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".exomonad")).unwrap();
    crate::exomonad::write_fixture_project_config(
        &project.path().join(".exomonad"),
        "gpt-6-sol",
        |_| {},
    );
    let selected =
        crate::exomonad::workspace::FrozenWorkspace::load(project.path(), run.path()).unwrap();
    let sources = driver_sources(
        Path::new("unused-live-actors"),
        Some(&selected),
        run.path(),
        None,
    )
    .unwrap();
    assert_eq!(sources.include[2], selected.runtime_actors());
    assert_eq!(sources.include[3], selected.runtime_stdlib());
}

fn write_bootstrap_workspace(workspace: &Path, spec: &str) {
    let authored = workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec!["Project.BootstrapWitness".into()];
        project.haskell.spec = Some("ConfiguredSpec.agentSpec".into());
    });
    std::fs::write(authored.join("ConfiguredSpec.hs"), spec).unwrap();
    std::fs::write(
        authored.join("Project/BootstrapWitness.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/bootstrap_workspace_witness.hs",
        ),
    )
    .unwrap();
}

#[test]
fn driver_sources_keep_policy_private_and_preserve_explicit_public_modules() {
    let project = tempfile::tempdir().unwrap();
    let authored = project.path().join(".exomonad");
    let spec = tidepool_testing::fixture_source(
        "bridge/facade/src/actor_host/fixtures/configured_bootstrap_spec.hs",
    );
    write_bootstrap_workspace(project.path(), &spec);
    std::fs::write(
        authored.join("AgentSpec.hs"),
        spec.replace("ConfiguredSpec", "AgentSpec"),
    )
    .unwrap();
    for expose_policy in [false, true] {
        crate::exomonad::write_fixture_project_config(&authored, "test-model", |config| {
            config.haskell.source_roots = vec![".".into()];
            config.haskell.modules = vec!["Project.BootstrapWitness".into()];
            if expose_policy {
                config.haskell.modules.push("ConfiguredSpec".into());
            }
            config.haskell.spec = Some("ConfiguredSpec.agentSpec".into());
        });
        let run = tempfile::tempdir().unwrap();
        let selected =
            crate::exomonad::workspace::FrozenWorkspace::load(project.path(), run.path()).unwrap();
        assert!(selected.provides_module("ConfiguredSpec"));
        assert!(selected.provides_module("AgentSpec"));
        let sources = driver_sources(
            Path::new("unused-live-actors"),
            Some(&selected),
            run.path(),
            None,
        )
        .unwrap();
        let templates =
            resident_workbench_templates(&sources.workbench_preamble, DRIVER_EFFECTS, "");
        let template = templates
            .iter()
            .find(|template| template.kind == tidepool_runtime::session::TemplateSelector::Expr)
            .unwrap();
        let notebook =
            tidepool_runtime::session::render_template(&template.source, DRIVER_ENTRY, &[]);
        assert!(notebook.contains("import Project.BootstrapWitness"));
        assert_eq!(notebook.contains("import ConfiguredSpec"), expose_policy);
        assert!(!notebook.contains("import qualified ConfiguredSpec"));
        assert!(!notebook.contains("import AgentSpec"));
        assert!(!notebook.contains("import qualified AgentSpec"));
        assert!(!sources.bootstrap_preamble.contains("ConfiguredSpec"));
        assert!(!sources
            .bootstrap_preamble
            .contains("Project.BootstrapWitness"));
    }
}

#[test]
fn runtime_driver_excludes_workspace_but_check_and_init_validate_configured_spec() {
    let project = tempfile::tempdir().unwrap();
    let valid_run = tempfile::tempdir().unwrap();
    let invalid_run = tempfile::tempdir().unwrap();
    write_bootstrap_workspace(
        project.path(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/configured_bootstrap_spec.hs",
        ),
    );
    let valid = crate::exomonad::workspace::FrozenWorkspace::load(project.path(), valid_run.path())
        .unwrap();
    // Public check/init use this full-workspace validation owner.
    validate_workspace_program(&valid, valid_run.path()).unwrap();

    write_bootstrap_workspace(
        project.path(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/invalid_configured_bootstrap_spec.hs",
        ),
    );
    let invalid =
        crate::exomonad::workspace::FrozenWorkspace::load(project.path(), invalid_run.path())
            .unwrap();
    let bootstrap = compile_driver(
        Path::new("unused-live-actors"),
        Some(&invalid),
        invalid_run.path(),
        None,
        DriverCompilePurpose::Bootstrap,
    )
    .expect("the fixed driver has no dependency on the invalid workspace spec");
    let certification = bootstrap.compiled.certification.as_ref().unwrap();
    assert!(certification.recovery_products.iter().all(|product| {
        !matches!(
            product.owner().module.as_str(),
            "ConfiguredSpec" | "Project.BootstrapWitness"
        )
    }));
    assert!(bootstrap
        .preamble
        .contains("import Project.BootstrapWitness"));
    assert!(!bootstrap
        .preamble
        .contains("import qualified ConfiguredSpec"));
    let sources = driver_sources(
        Path::new("unused-live-actors"),
        Some(&invalid),
        invalid_run.path(),
        None,
    )
    .unwrap();
    assert_eq!(bootstrap.include, sources.include);
    let failure = validate_workspace_program(&invalid, invalid_run.path()).unwrap_err();
    assert!(
        failure.to_string().contains("missingConfiguredStartupSpec"),
        "{failure}"
    );
    assert!(
        failure.to_string().contains("ConfiguredSpec.hs:6:13"),
        "{failure}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_runtime_launch_refuses_invalid_configured_spec_before_ready() {
    let files = tempfile::tempdir().unwrap();
    let settings = test_campaign::hosted_test_settings(&files, 1);
    let (provider, _requests) = test_campaign::hosted_script_provider();
    let configure = |config: &mut ActorHostConfig, spec: &str| {
        write_bootstrap_workspace(&config.workspace, spec);
        test_campaign::commit_workspace(&config.workspace);
        config.workspace_inputs = Some(
            crate::exomonad::workspace::FrozenWorkspace::load(
                &config.workspace,
                &config.run_directory.path(),
            )
            .unwrap(),
        );
    };
    let valid =
        hosted_test_context::HostedTestRuntime::start_configured(&settings, &provider, |config| {
            configure(
                config,
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/configured_bootstrap_spec.hs",
                ),
            )
        })
        .await
        .expect("the valid configured spec reaches production root readiness");
    let valid_root = valid.context.actor.identity();
    valid
        .stop()
        .await
        .expect("valid control cleanup is confirmed");
    let result =
        hosted_test_context::HostedTestRuntime::start_configured(&settings, &provider, |config| {
            configure(
                config,
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/invalid_configured_bootstrap_spec.hs",
                ),
            );
        })
        .await;
    match result {
        Ok(host) => {
            host.stop().await.unwrap();
            panic!("invalid configured spec reached production root readiness");
        }
        Err(failure) => {
            let (actor, terminal) = failure
                .root_terminal()
                .expect("original failed root retained");
            assert_ne!(
                actor, valid_root,
                "the refusal belongs to the new admitted root"
            );
            assert_eq!(actor.incarnation, exomonad_actor::Incarnation::FIRST);
            assert_eq!(terminal.kind, exomonad_actor::ActorExitKind::Failed);
            let diagnostic = terminal
                .diagnostic
                .as_ref()
                .expect("original compiler failure retained");
            assert_eq!(
                diagnostic.class,
                tidepool_toolchain::failclass::FailureClass::UserHaskell
            );
            assert_eq!(
                diagnostic.phase,
                tidepool_toolchain::failclass::Phase::Compile
            );
            assert_eq!(
                diagnostic.cause,
                Some(tidepool_toolchain::failclass::CompileFailureCause::SourceDiagnostics)
            );
            assert!(
                diagnostic.message.contains("missingConfiguredStartupSpec"),
                "{diagnostic:?}"
            );
            assert!(failure.cleanup_confirmed(), "{failure}");
        }
    }
}

#[test]
fn later_host_before_root_admission_creates_missing_journals() {
    let directory = tempfile::tempdir().unwrap();
    let binding = directory.path().join("root-binding.json");
    let actors = directory.path().join("actor-lifecycle.v2.jsonl");
    let run = directory.path().join("run-journal.jsonl");
    let incarnation = exomonad_actor::Incarnation(2);

    assert_eq!(
        actor_journal_mode(incarnation, &binding, &actors, &run).unwrap(),
        JournalOpenMode::Create
    );
    let actor_journal = exomonad_actor::ActorRecoveryJournal::open(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path()).unwrap(),
        "actor-lifecycle.v2.jsonl",
    )
    .unwrap();
    assert!(actor_journal.records().is_empty());
    assert_eq!(
        run_journal_mode(incarnation, &binding, &run, true).unwrap(),
        JournalOpenMode::Create
    );
    tidepool_handlers::SegmentPath::create_exclusive(run.clone()).unwrap();

    assert_eq!(
        actor_journal_mode(incarnation, &binding, &actors, &run).unwrap(),
        JournalOpenMode::Resume
    );
    assert_eq!(
        run_journal_mode(incarnation, &binding, &run, true).unwrap(),
        JournalOpenMode::Resume
    );
}

#[test]
fn later_host_requires_missing_journals_when_prior_evidence_exists() {
    let directory = tempfile::tempdir().unwrap();
    let binding = directory.path().join("root-binding.json");
    let actors = directory.path().join("actor-lifecycle.v2.jsonl");
    let run = directory.path().join("run-journal.jsonl");
    let incarnation = exomonad_actor::Incarnation(2);

    std::fs::write(&binding, b"prior root was bound").unwrap();
    assert_eq!(
        actor_journal_mode(incarnation, &binding, &actors, &run).unwrap(),
        JournalOpenMode::Resume
    );
    assert!(exomonad_actor::ActorRecoveryJournal::open_existing(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path()).unwrap(),
        "actor-lifecycle.v2.jsonl"
    )
    .is_err());
    assert_eq!(
        run_journal_mode(incarnation, &binding, &run, true).unwrap(),
        JournalOpenMode::Resume
    );
    assert!(tidepool_handlers::SegmentPath::open_existing(run.clone()).is_err());

    std::fs::remove_file(&binding).unwrap();
    std::fs::write(&run, b"prior run journal").unwrap();
    assert_eq!(
        actor_journal_mode(incarnation, &binding, &actors, &run).unwrap(),
        JournalOpenMode::Resume
    );
    std::fs::remove_file(&run).unwrap();
    assert_eq!(
        run_journal_mode(incarnation, &binding, &run, false).unwrap(),
        JournalOpenMode::Resume
    );
}

#[test]
fn root_never_bound_is_true_exactly_when_the_binding_file_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let binding_path = root.path().join("root-binding.json");
    assert!(root_never_bound(None, &binding_path));

    std::fs::write(
        &binding_path,
        "not even a real binding, just proof of writing",
    )
    .unwrap();
    assert!(!root_never_bound(None, &binding_path));
}

#[test]
fn embedded_application_binding_does_not_require_native_launch_proof() {
    use exomonad_actor::{ApplicationConversation, DurableActorApplication};
    let root = tempfile::tempdir().unwrap();
    let binding_path = root.path().join("root-binding.json");
    let conversation = ApplicationConversation::Embedded {
        run: "run".into(),
        agent_path: "/root".into(),
        incarnation: "1".into(),
    };
    assert!(!native_exit_required(Some(&conversation)));
    assert!(native_exit_required(None));
    assert!(native_exit_required(Some(
        &ApplicationConversation::Codex {
            thread_id: "thread".into()
        }
    )));
    let mut application = DurableActorApplication {
        binding_path: binding_path.clone(),
        conversation: Some(conversation.clone()),
        intended_conversation: Some(conversation),
        accepted_source: None,
    };
    assert!(!root_never_bound(Some(&application), &binding_path));
    application.conversation = None;
    assert!(root_never_bound(Some(&application), &binding_path));
}

#[test]
fn default_capabilities_include_journal() {
    let capabilities = exomonad_actor::ActorCapabilities::default();
    assert!(capabilities
        .effect_keys()
        .contains(&exomonad_actor::ActorEffectKey::Journal));
    assert!(capabilities.haskell_effects_type().contains("Journal"));
}

#[test]
fn typed_site_surface_callers_have_returning_contracts() {
    use tidepool_repr::execution_schema::{Group, HeapRhs, ResultContract, RuntimeRep};

    tidepool_testing::eval_harness::require_extract();
    let haskell = crate::haskell_sources::ensure_exomonad_haskell().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let sources = driver_sources(&haskell, None, directory.path(), None).unwrap();
    let names = [
        "receiveProbe",
        "requestProbe",
        "progressProbe",
        "retainedProgressProbe",
        "childProbe",
        "childProgressProbe",
    ];
    let artifacts = tidepool_runtime::compile_targets(
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/typed_site_return_contract.hs",
        ),
        &names,
        &sources.include,
        |_, _, _| {},
    )
    .unwrap();
    for name in names {
        let program = artifacts.targets[name].prepared.prepared();
        let entry = program
            .bindings()
            .iter()
            .flat_map(|group| match group {
                Group::NonRecursive(binding) => std::slice::from_ref(binding),
                Group::Recursive(bindings) => bindings.as_slice(),
            })
            .find(|binding| binding.binding.id == program.entry())
            .unwrap();
        let signature = match entry.binding.rhs {
            HeapRhs::Function { signature, .. } | HeapRhs::Thunk { signature, .. } => {
                &program.signatures()[signature.0 as usize]
            }
            ref other => panic!("{name} has no callable entry: {other:?}"),
        };
        assert_eq!(
            signature.results,
            ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            "{name}: site elaboration must preserve a returning caller"
        );
        assert!(!program.sites().is_empty(), "{name}: expected a typed site");
    }
}

#[tokio::test]
async fn roster_observation_preserves_host_and_sibling_workbenches() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let setup = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/roster_setup.hs",
                    ),
                )
                .await;
                assert_eq!(setup["status"], "committed", "{setup:?}");
                let mut children = Vec::new();
                while children.len() < 2 {
                    let child = campaign
                        .next_deployment(
                            "roster child admission",
                            Duration::from_secs(30),
                            |event| match event {
                                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                                LocalResidentDeployment::Retired { actor, terminal } => {
                                    panic!("{actor:?}: {terminal:?}")
                                }
                                other => Err(other),
                            },
                        )
                        .await;
                    children.push(child);
                }
                for policy in std::iter::once(root.as_ref())
                    .chain(children.iter().map(|child| child.policy.as_ref()))
                {
                    let observed = dispatch_haskell_script(
                        policy,
                        &tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/roster_observe.hs",
                        ),
                    )
                    .await;
                    assert_eq!(observed["status"], "committed", "{observed:?}");
                    let roster_type = dispatch_lookup(policy, &["AgentRosterEntry"]).await;
                    assert!(
                        roster_type["items"][0]["output"]
                            .as_str()
                            .is_some_and(|output| output.contains("rosterActorId")),
                        "{roster_type:?}"
                    );
                    let next = dispatch_haskell_script(policy, "40 + 2 :: Int").await;
                    assert_eq!(next["status"], "committed", "{next:?}");
                    assert_eq!(next["items"][0]["output"], "42", "{next:?}");
                }
                let status = dispatch_status(root.as_ref(), "detailed").await;
                assert_ne!(status["status"], "failed", "{status:?}");
            })
        })
        .await;
}

#[tokio::test]
async fn root_recovery_replays_lost_workbench_reply_without_repeating_effects() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let target = campaign
        .forest
        .new_workbench(
            "notification-target".into(),
            exomonad_actor::ActorCapabilities::default(),
        )
        .await
        .unwrap();
    let target_id = target.identity();
    let source = format!(
        "import qualified Tidepool.Effects.Core as RecoveryEffects\nsend (RecoveryEffects.NotifyWith ({}, {}) \"counted-recovery-effect\") >> pure ()",
        target_id.id.0, target_id.incarnation.0
    );
    let request = ToolInvocation {
        context: Some(ToolInvocationContext::external(
            "retained-native-thread".into(),
            "native-turn".into(),
            "native-call".into(),
            Some("lost-recovery-call".into()),
            None,
        )),
        name: exomonad_actor::HASKELL_TOOL.into(),
        arguments: exomonad_tool::ToolArguments::Raw(source),
    };
    let policy = campaign.root_installation.policy.clone();
    let mut first = tokio::spawn(policy.dispatch_json_boxed(request.clone()));
    let mut effects = 0;
    let notification = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::select! {
            result = &mut first => panic!("effect was not dispatched: {result:?}"),
            command = campaign.next_deployment(
                "recovery notification",
                Duration::from_secs(60),
                |event| match event {
                    LocalResidentDeployment::NotificationSend(command) => Ok(command),
                    other => Err(other),
                },
            ) => {
                effects += 1;
                command
            }
        }
    })
    .await
    .unwrap();
    first.abort();
    // best-effort: task is aborted; the join result is expected to be Cancelled.
    first.await.ok();
    notification.admitted("recovery-test-inbox".into(), 1);
    let retained = tokio::time::timeout(
        Duration::from_secs(60),
        policy.dispatch_json_boxed(request.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(retained["status"], "committed", "{retained:?}");
    campaign
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "recover after losing the native reply".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    campaign.observe_hosted_completion().await.unwrap();
    let (successor, task) = campaign
        .forest
        .recover_program_root(
            campaign.actor.identity(),
            "recovered-root".into(),
            exomonad_actor::ActorCapabilities::default(),
            campaign.program.clone(),
        )
        .await
        .unwrap();
    assert_ne!(successor.identity(), campaign.actor.identity());
    assert!(
        campaign
            .forest
            .recover_program_root(
                campaign.actor.identity(),
                "duplicate-recovery".into(),
                exomonad_actor::ActorCapabilities::default(),
                campaign.program.clone(),
            )
            .await
            .is_err()
    );
    let successor_identity = successor.identity();
    let successor_installation = campaign
        .next_deployment(
            "recovered root installed policy",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation)
                    if installation.actor.identity() == successor_identity =>
                {
                    Ok(installation)
                }
                LocalResidentDeployment::Retired { actor, terminal }
                    if actor == successor_identity =>
                {
                    panic!("recovered root retired before installing its policy: {terminal:?}")
                }
                other => Err(other),
            },
        )
        .await;
    let successor_policy = successor_installation.policy;
    assert!(successor_policy
        .tools()
        .iter()
        .any(|tool| tool.name() == exomonad_actor::HASKELL_TOOL));
    let mut retry = tokio::spawn(successor_policy.dispatch_json_boxed(request.clone()));
    let replay = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            tokio::select! {
                result = &mut retry => break result.unwrap().unwrap(),
                command = campaign.next_deployment(
                    "recovery replay notification",
                    Duration::from_secs(60),
                    |event| match event {
                        LocalResidentDeployment::NotificationSend(command) => Ok(command),
                        other => Err(other),
                    },
                ) => {
                    effects += 1;
                    command.admitted("recovery-test-inbox".into(), effects);
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(effects, 1, "recovery must not dispatch the effect again");
    assert_eq!(
        replay, retained,
        "replay preserves the original execution receipt"
    );
    let mut altered = request;
    altered.arguments = exomonad_tool::ToolArguments::Raw("pure (99 :: Int)".into());
    let conflict = successor_policy
        .dispatch_json_boxed(altered)
        .await
        .unwrap_err();
    assert!(conflict.to_string().contains("different Haskell input"));
    campaign.observe_forest_shutdown().await;
    task.await.unwrap();

    })).await;
}

#[tokio::test]
async fn actor_sources_capture_current_then_deliver_every_publication_and_settlement() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/source_setup.hs"),
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let child = campaign
        .next_deployment(
            "source child admission",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    let first = dispatch_haskell_script(
        child.policy.as_ref(),
        "reportProgress (ProgressNote 1 (+ sessionInput))",
    )
    .await;
    assert_eq!(first["status"], "committed", "{first:?}");
    let installed = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/source_actor.hs"),
    )
    .await;
    assert_eq!(installed["status"], "committed", "{installed:?}");
    for item in installed["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{installed:?}");
    }
    let rejected = dispatch_haskell_script_result(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/source_replacement_rejected.hs",
        ),
    )
    .await
    .expect_err("replacement cannot change the source graph");
    assert!(
        rejected
            .to_string()
            .contains("replacement removed a source"),
        "{rejected:?}"
    );
    let replaced = dispatch_haskell_script(
        root.as_ref(),
        "collector2 <- replaceActor collector collectorDefinition",
    )
    .await;
    assert_eq!(replaced["status"], "committed", "{replaced:?}");
    let published = dispatch_haskell_script(child.policy.as_ref(), "reportProgress (ProgressNote 2 (* sessionInput))\nreportProgress (ProgressNote 3 (subtract sessionInput))\nrespond (42 :: Int)").await;
    assert_eq!(published["status"], "replied", "{published:?}");
    let settled = dispatch_haskell_script(root.as_ref(), "settled <- watch Nothing (result answer)").await;
    assert_eq!(settled["status"], "committed", "{settled:?}");
    campaign.await_watch_ready().await;
    let collected = dispatch_haskell_script(
        root.as_ref(),
        "drainActor collector2\nresult <- awaitExit collector2\ncase result of { Completed values -> reverse values == [13, 30, -7, -1, 42]; _ -> False }",
    )
    .await;
    assert_eq!(collected["status"], "committed", "{collected:?}");
    assert!(collected.to_string().contains("True"), "{collected:?}");
})).await;
}

#[tokio::test]
async fn progress_retains_closures_and_watch_snapshots_across_calls() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let setup = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/progress_setup.hs",
                    ),
                )
                .await;
                assert_eq!(setup["status"], "committed", "{setup:?}");
                let child = campaign
                    .next_deployment(
                        "progress child policy installation",
                        Duration::from_secs(30),
                        |event| match event {
                            LocalResidentDeployment::PolicyInstalled(installation) => {
                                Ok(installation)
                            }
                            other => Err(other),
                        },
                    )
                    .await;
                campaign
                    .next_deployment(
                        "progress request activation",
                        Duration::from_secs(30),
                        |event| match event {
                            LocalResidentDeployment::SessionReady { .. } => Ok(()),
                            other => Err(other),
                        },
                    )
                    .await;
                let first = dispatch_haskell_script(
                    child.policy.as_ref(),
                    "reportProgress (ProgressNote 1 (+ sessionInput))",
                )
                .await;
                assert_eq!(first["status"], "committed", "{first:?}");
                campaign.await_watch_ready().await;
                let second = dispatch_haskell_script(
                    child.policy.as_ref(),
                    "reportProgress (ProgressNote 2 (* sessionInput))",
                )
                .await;
                assert_eq!(second["status"], "committed", "{second:?}");
                let captured = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/progress_observe.hs",
                    ),
                )
                .await;
                assert_eq!(captured["status"], "committed", "{captured:?}");
                assert!(captured.to_string().contains("(13, 30)"), "{captured:?}");
                assert_eq!(captured["items"][7]["output"], "40", "{captured:?}");
                let reply =
                    dispatch_haskell_script(child.policy.as_ref(), "respond (42 :: Int)").await;
                assert_eq!(reply["status"], "replied", "{reply:?}");
                let stopped = dispatch_haskell_script(root.as_ref(), "stopAgent worker").await;
                assert_eq!(stopped["status"], "committed", "{stopped:?}");
                let retained = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/progress_retained.hs",
                    ),
                )
                .await;
                assert_eq!(retained["status"], "committed", "{retained:?}");
                assert_eq!(retained["items"][1]["output"], "True", "{retained:?}");
                assert_eq!(retained["items"][2]["output"], "15", "{retained:?}");
                assert_eq!(retained["items"][4]["output"], "50", "{retained:?}");
                assert_eq!(retained["items"][6]["output"], "16", "{retained:?}");
                campaign
                    .actor
                    .shutdown(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "progress test complete".into(),
                        diagnostic: None,
                    })
                    .await
                    .unwrap();
                campaign.observe_hosted_completion().await.unwrap();
            })
        })
        .await;
}

fn durable_root(actor: ActorRef) -> exomonad_actor::DurableActorRecord {
    exomonad_actor::DurableActorRecord {
        admission: exomonad_actor::DurableActorAdmission {
            actor,
            label: Some("exomonad-root".into()),
            creator: None,
            supervisor_parent: None,
            context_parent: None,
            actor_path: None,
            effect_keys: exomonad_actor::ActorCapabilities::default()
                .effect_keys()
                .to_vec(),
            descendant_depth: 8,
            descendant_active_children: None,
            model: None,
            effort: None,
            instructions: None,
            launch_worktrees: Vec::new(),
            source_layer: Vec::new(),
        },
        startup: None,
        application: Some(exomonad_actor::DurableActorApplication {
            binding_path: std::path::PathBuf::from(format!(
                "binding-{}-{}.json",
                actor.id.0, actor.incarnation.0
            )),
            conversation: Some(format!("conversation-{}", actor.id.0).into()),
            intended_conversation: None,
            accepted_source: Some("source-revision".into()),
        }),
        terminal: None,
    }
}

#[test]
fn actor_recovery_records_the_published_source_revision() {
    let project = tempfile::tempdir().unwrap();
    let run = tempfile::tempdir().unwrap();
    let authored = project.path().join(".exomonad/Project");
    std::fs::create_dir_all(&authored).unwrap();
    crate::exomonad::write_fixture_project_config(
        &project.path().join(".exomonad"),
        "gpt-6-sol",
        |project| {
            project.haskell.source_roots = vec![".".into()];
            project.haskell.modules = vec!["Project.Work".into()];
        },
    );
    std::fs::write(
        authored.join("Work.hs"),
        "module Project.Work where\nwork :: Int\nwork = 1\n",
    )
    .unwrap();
    let frozen =
        crate::exomonad::workspace::FrozenWorkspace::load(project.path(), run.path()).unwrap();
    let layer = crate::exomonad::source::SourceLayer::new(run.path());
    let first = layer.ensure_active(&frozen).unwrap();

    assert_eq!(
        active_source_identity(run.path(), true).unwrap(),
        Some(first.identity.clone())
    );
    assert_ne!(first.identity, frozen.identity());

    std::fs::write(
        project.path().join(".exomonad/Project/Work.hs"),
        "module Project.Work where\nwork :: Int\nwork = 2\n",
    )
    .unwrap();
    let second = layer
        .publish(
            layer
                .capture_from_workspace(&frozen, project.path())
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        active_source_identity(run.path(), true).unwrap(),
        Some(second.identity)
    );
    assert_eq!(active_source_identity(run.path(), false).unwrap(), None);
}

#[test]
fn recovery_preserves_root_logical_id_and_advances_actor_incarnation() {
    let first = ActorRef::first(exomonad_actor::ActorId(7));
    let second = ActorRef {
        id: first.id,
        incarnation: exomonad_actor::Incarnation(2),
    };
    let records = vec![durable_root(first), durable_root(second)];
    assert_eq!(
        durable_root_identity(&records, Some("source-revision")).unwrap(),
        Some((
            second,
            ActorRef {
                id: first.id,
                incarnation: exomonad_actor::Incarnation(3),
            }
        ))
    );
}

#[test]
fn root_recovery_ignores_operator_with_root_privileges() {
    let root = ActorRef::first(exomonad_actor::ActorId(1));
    let mut operator = durable_root(ActorRef::first(exomonad_actor::ActorId(2)));
    operator.admission.label = Some("operator".into());
    operator.application = None;
    for records in [
        vec![durable_root(root), operator.clone()],
        vec![operator, durable_root(root)],
    ] {
        assert_eq!(
            durable_root_identity(&records, Some("source-revision"))
                .unwrap()
                .unwrap()
                .0,
            root
        );
    }
}

#[test]
fn root_recovery_does_not_skip_successor_without_application() {
    let first = ActorRef::first(exomonad_actor::ActorId(1));
    let mut successor = durable_root(next_actor_incarnation(first).unwrap());
    successor.application = None;
    assert_eq!(
        durable_root_identity(&[durable_root(first), successor], Some("source-revision")).unwrap(),
        None
    );
}

#[test]
fn root_recovery_refuses_ambiguous_application_owners() {
    let records = [
        durable_root(ActorRef::first(exomonad_actor::ActorId(1))),
        durable_root(ActorRef::first(exomonad_actor::ActorId(2))),
    ];
    assert!(durable_root_identity(&records, Some("source-revision")).is_err());
}

#[test]
fn root_recovery_does_not_fall_back_past_incomplete_latest_evidence() {
    let first = ActorRef::first(exomonad_actor::ActorId(7));
    let second = ActorRef {
        id: first.id,
        incarnation: exomonad_actor::Incarnation(2),
    };
    let mut latest = durable_root(second);
    latest.application.as_mut().unwrap().accepted_source = Some("different-source".into());

    assert_eq!(
        durable_root_identity(&[durable_root(first), latest], Some("source-revision")).unwrap(),
        None
    );
}

#[test]
fn root_recovery_does_not_fall_back_past_a_retired_incarnation() {
    let first = ActorRef::first(exomonad_actor::ActorId(7));
    let second = ActorRef {
        id: first.id,
        incarnation: exomonad_actor::Incarnation(2),
    };
    let mut latest = durable_root(second);
    latest.terminal = Some(exomonad_actor::DurableActorTerminal::new(
        exomonad_actor::ActorExitKind::Completed,
        "retired",
    ));

    assert_eq!(
        durable_root_identity(&[durable_root(first), latest], Some("source-revision")).unwrap(),
        None
    );
}

#[test]
fn crash_before_root_admission_has_no_identity_to_adopt() {
    let records = Vec::new();
    assert!(!contains_durable_root_admission(&records));
    assert_eq!(
        durable_root_identity(&records, Some("source-revision")).unwrap(),
        None
    );
}

#[test]
fn recovery_keeps_unverifiable_children_unavailable_without_fencing_the_root() {
    let run = tempfile::tempdir().unwrap();
    let root = run.path().join("1-1");
    let child = run.path().join("2-1");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    tidepool_atomic_write::write_durable(
        &root.join(PROCESS_RECOVERY_RECORD),
        &serde_json::to_vec(&ProcessRecoveryRecord {
            version: 1,
            launch_id: "root-launch".into(),
            recovery_secret: "retired".into(),
            supervisor_socket: root.join("supervisor.sock"),
            socket_root: root.join("sockets"),
            retired: true,
        })
        .unwrap(),
    )
    .unwrap();
    std::fs::write(child.join(PROCESS_RECOVERY_RECORD), b"not-json").unwrap();

    let report = stop_predecessor_processes(run.path()).unwrap();
    assert!(report.root_available());
    assert_eq!(report.unavailable_names(), vec!["2-1"]);
}

#[test]
fn recovery_retries_socket_cleanup_after_a_durable_retirement_marker() {
    let run = tempfile::tempdir().unwrap();
    let actor = run.path().join("1-1");
    let socket_root = actor.join("sockets");
    std::fs::create_dir_all(&socket_root).unwrap();
    std::fs::write(socket_root.join("stale.sock"), b"stale").unwrap();
    tidepool_atomic_write::write_durable(
        &actor.join(PROCESS_RECOVERY_RECORD),
        &serde_json::to_vec(&ProcessRecoveryRecord {
            version: 1,
            launch_id: "root-launch".into(),
            recovery_secret: "retired".into(),
            supervisor_socket: socket_root.join("supervisor.sock"),
            socket_root: socket_root.clone(),
            retired: true,
        })
        .unwrap(),
    )
    .unwrap();

    let report = stop_predecessor_processes(run.path()).unwrap();

    assert_eq!(report.stopped, 0, "retired evidence must not stop twice");
    assert!(report.root_available());
    assert!(!socket_root.exists());
}

#[test]
fn recovery_fails_closed_when_root_process_evidence_is_missing() {
    let run = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(run.path().join("1-1")).unwrap();
    let report = stop_predecessor_processes(run.path()).unwrap();
    assert!(!report.root_available());
    assert_eq!(report.unavailable_names(), vec!["1-1"]);
}

#[test]
fn recovery_treats_a_numeric_root_directory_with_leading_zero_as_root() {
    let run = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(run.path().join("01-2")).unwrap();
    std::fs::create_dir_all(run.path().join("notes-2")).unwrap();

    let report = stop_predecessor_processes(run.path()).unwrap();
    assert!(!report.root_available());
    assert_eq!(report.unavailable_names(), vec!["01-2"]);
}

#[test]
fn recovery_reports_a_stopped_child_until_its_actor_state_can_be_rebuilt() {
    let run = tempfile::tempdir().unwrap();
    let root = run.path().join("1-1");
    let child = run.path().join("2-1");
    let socket_root = child.join("sockets");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&socket_root).unwrap();
    tidepool_atomic_write::write_durable(
        &root.join(PROCESS_RECOVERY_RECORD),
        &serde_json::to_vec(&ProcessRecoveryRecord {
            version: 1,
            launch_id: "root-launch".into(),
            recovery_secret: "retired".into(),
            supervisor_socket: root.join("supervisor.sock"),
            socket_root: root.join("sockets"),
            retired: true,
        })
        .unwrap(),
    )
    .unwrap();
    let supervisor_socket = socket_root.join("supervisor.sock");
    tidepool_atomic_write::write_durable(
        &child.join(PROCESS_RECOVERY_RECORD),
        &serde_json::to_vec(&ProcessRecoveryRecord {
            version: 1,
            launch_id: "child-launch".into(),
            recovery_secret: "secret".into(),
            supervisor_socket: supervisor_socket.clone(),
            socket_root: socket_root.clone(),
            retired: false,
        })
        .unwrap(),
    )
    .unwrap();
    tidepool_atomic_write::write_durable(
        &socket_root.join(exomonad_node::PROCESS_SUPERVISOR_CHECKPOINT),
        &serde_json::to_vec(&ProcessRecoveryCheckpoint {
            version: exomonad_node::PROCESS_SUPERVISOR_VERSION,
            launch_id: "child-launch".into(),
            observation: exomonad_node::ProcessSupervisorObservation::ProcessStopped,
            operation_pending: false,
            error: None,
        })
        .unwrap(),
    )
    .unwrap();

    let report = stop_predecessor_processes(run.path()).unwrap();
    assert!(report.root_available());
    assert_eq!(report.stopped, 1);
    assert_eq!(report.unavailable_names(), vec!["2-1"]);
    assert!(!socket_root.exists());
}

#[test]
fn notification_receipt_provenance_keeps_legacy_untagged_shape() {
    let sender = ActorRef::first(exomonad_actor::ActorId(7));
    let target = ActorRef::first(exomonad_actor::ActorId(8));
    let encoded =
        serde_json::to_value(DeliveryProvenance::Notification { sender, target }).unwrap();
    assert!(encoded.get("kind").is_none());
    assert_eq!(
        serde_json::from_value::<DeliveryProvenance>(encoded).unwrap(),
        DeliveryProvenance::Notification { sender, target }
    );
}

async fn dispatch_haskell(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    items: impl IntoIterator<Item = &'static str>,
) -> serde_json::Value {
    let mut last = None;
    for item in items {
        let result = dispatch_haskell_script(endpoint, item).await;
        assert_ne!(
            result["status"], "rejected",
            "Haskell item rejected:\n{item}\n\n{result:?}\n\nprevious receipt: {last:?}"
        );
        last = Some(result);
    }
    last.expect("non-empty Haskell fixture")
}

#[test]
fn root_and_worker_share_git_metadata_but_not_working_tree_authority() {
    let source = Path::new("/source");
    let worker = Path::new("/workers/one");
    let common = Path::new("/source/.git");

    assert_eq!(
        writable_repository_roots(
            true,
            exomonad_actor::WorkspaceAccess::ReadWrite,
            source,
            None,
            common,
            None,
        ),
        vec![source.to_path_buf(), common.to_path_buf()]
    );
    assert_eq!(
        writable_repository_roots(
            false,
            exomonad_actor::WorkspaceAccess::ReadWrite,
            source,
            Some(worker),
            common,
            None,
        ),
        vec![worker.to_path_buf(), common.to_path_buf()]
    );
    assert_eq!(
        writable_repository_roots(
            false,
            exomonad_actor::WorkspaceAccess::ReadOnly,
            source,
            Some(worker),
            common,
            None,
        ),
        Vec::<PathBuf>::new()
    );
}

/// The root has to be able to run the project's own build in a worktree it
/// allocated for itself. Children's worktrees are a different directory and
/// stay read-only to the root.
#[test]
fn the_root_may_build_in_its_own_worktrees_but_not_in_a_child_s() {
    let source = Path::new("/source");
    let common = Path::new("/source/.git");
    let managed = Path::new("/state/actor-worktrees/p/worktrees");
    let root_worktrees = managed.join(WorktreeManager::ROOT_ALLOCATION_DIR);

    let writable = writable_repository_roots(
        true,
        exomonad_actor::WorkspaceAccess::ReadWrite,
        source,
        None,
        common,
        Some(&root_worktrees),
    );
    assert!(
        writable.contains(&root_worktrees),
        "the root's own allocations are writable to it: {writable:?}"
    );
    assert!(
        root_worktrees.starts_with(managed),
        "the root directory must nest inside the managed root, which is the boundary's read-only root"
    );
    assert!(
        !writable.iter().any(|path| path == managed),
        "a child's worktree stays read-only to the root: {writable:?}"
    );
    assert!(
        !writable_repository_roots(
            false,
            exomonad_actor::WorkspaceAccess::ReadWrite,
            source,
            Some(&managed.join("wt-child")),
            common,
            Some(&root_worktrees),
        )
        .contains(&root_worktrees),
        "a child never receives the root's allocation directory"
    );
}

/// A fresh-context child inherits no model context, but it still has a
/// supervisor: `parentAgent` names it, and `sendMessage` to that handle lands
/// in the supervisor's tracked inbox.
#[tokio::test]
async fn fresh_context_child_reaches_its_supervisor_through_parent_agent() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let output_store = display_output::open_run_store(campaign.session_root.path()).unwrap();
    let root_id = campaign.actor.identity();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/notification_setup.hs"),
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let child = campaign
        .next_deployment(
            "fresh-context child policy",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(child.context_parent, None, "FreshCtx selects its context");
    assert_eq!(child.supervisor_parent, Some(root_id));
    campaign
        .next_deployment(
            "fresh-context child activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let supervisor = dispatch_haskell_script(
        child.policy.as_ref(),
        &format!(
            "fmap (\\c -> contextSupervisorId c == Just {} && contextSupervisorIncarnation c == Just {}) actorContext",
            root_id.id.0, root_id.incarnation.0
        ),
    )
    .await;
    assert_eq!(supervisor["status"], "committed", "{supervisor:?}");
    assert_eq!(supervisor["items"][0]["output"], "True", "{supervisor:?}");
    let policy = child.policy.clone();
    let send = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            "Just parent <- parentAgent\nRight receipt <- sendMessage parent \"checkpoint\"",
        )
        .await
    });
    let command = campaign
        .next_deployment(
            "child notification to its supervisor",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationSend(command) => Ok(command),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(command.owner(), child.actor.identity());
    assert_eq!(command.target(), root_id);
    let directory = tempfile::tempdir().unwrap();
    let inbox = ActorInbox::open(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path()).unwrap(),
        "rows",
        "cursor",
    )
    .unwrap();
    let inbox_key = "supervisor-inbox";
    admit_notification(&command, inbox_key.into(), &inbox);
    let sent = send.await.unwrap();
    assert_eq!(sent["status"], "committed", "{sent:?}");
    let policy = child.policy.clone();
    let poll = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            "_ <- pollNotification receipt >>= display . show",
        )
        .await
    });
    let poll_command = campaign
        .next_deployment(
            "child notification poll",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationPoll(command) => Ok(command),
                other => Err(other),
            },
        )
        .await;
    let result = observe_notification_receipt(&poll_command, root_id, inbox_key, &inbox);
    assert_eq!(result, Ok(exomonad_actor::NotificationState::Accepted));
    poll_command.observed(result);
    let observed = campaign
        .drive_actor_output(&output_store, poll)
        .await
        .unwrap();
    assert!(
        observed.to_string().contains("NotificationAccepted"),
        "{observed:?}"
    );
})).await;
}

#[tokio::test]
async fn notification_admission_and_poll_preserve_typed_request_bindings() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let output_store = display_output::open_run_store(campaign.session_root.path()).unwrap();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/notification_setup.hs"),
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let child = campaign
        .next_deployment(
            "recipient policy",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    let activation = campaign
        .next_deployment(
            "original request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation } => Ok(activation),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(activation.id.actor(), child.actor.identity());
    let directory = tempfile::tempdir().unwrap();
    // Distinct fresh hierarchies exercise both strict directory owners at
    // the authored notification/inbox seam; syscall denial is tested by node.
    let inbox = ActorInbox::open(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path()).unwrap(),
        "rows-tree/deep/rows",
        "checkpoint-tree/deep/cursor",
    )
    .unwrap();
    let inbox_key = "notification-test-inbox";
    let policy = root.clone();
    let send = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            "Right receipt <- sendMessage worker \"one-way notice\"",
        )
        .await
    });
    let command = campaign
        .next_deployment(
            "notification send",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationSend(command) => Ok(command),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(command.owner(), campaign.actor.identity());
    assert_eq!(command.target(), child.actor.identity());
    admit_notification(&command, inbox_key.into(), &inbox);
    let sent = send.await.unwrap();
    assert_eq!(sent["status"], "committed", "{sent:?}");
    // `sendMessage`'s result reads as what happened, not as a wire tuple.
    let accepted = format!(
        " to agent {} accepted; `pollNotification` on this receipt reports whether it was presented",
        child.actor.identity()
    );
    for cell in [
        "receipt",
        "(Right receipt :: Either NotificationError NotificationReceipt)",
    ] {
        let shown = campaign
            .drive_actor_output(
                &output_store,
                dispatch_haskell_script(root.as_ref(), &format!("display ({cell})")),
            )
            .await;
        assert_eq!(shown["status"], "committed", "{shown:?}");
        let output = shown["items"][0]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|operation| operation.get("display"))
            .unwrap()["text"]
            .as_str()
            .unwrap();
        assert!(
            output.starts_with("notification ") && output.ends_with(&accepted),
            "{output}"
        );
    }
    let policy = root.clone();
    let poll = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            "_ <- pollNotification receipt >>= display . show",
        )
        .await
    });
    let poll_command = campaign
        .next_deployment(
            "notification poll",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationPoll(command) => Ok(command),
                other => Err(other),
            },
        )
        .await;
    let result =
        observe_notification_receipt(&poll_command, child.actor.identity(), inbox_key, &inbox);
    assert_eq!(result, Ok(exomonad_actor::NotificationState::Accepted));
    assert_eq!(
        observe_notification_receipt(
            &poll_command,
            child.actor.identity(),
            "foreign-inbox",
            &inbox
        ),
        Err(exomonad_actor::NotificationError::InvalidReceipt)
    );
    let foreign_directory = tempfile::tempdir().unwrap();
    let foreign = ActorInbox::open(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(foreign_directory.path()).unwrap(),
        "rows-tree/deep/rows",
        "checkpoint-tree/deep/cursor",
    )
    .unwrap();
    assert_eq!(
        observe_notification_receipt(&poll_command, child.actor.identity(), inbox_key, &foreign),
        Err(exomonad_actor::NotificationError::Unavailable)
    );
    foreign
        .publish_tracked(
            DurableActorEvent::Text("another sender".into()),
            DeliveryProvenance::Notification {
                sender: child.actor.identity(),
                target: child.actor.identity(),
            },
        )
        .unwrap();
    assert_eq!(
        observe_notification_receipt(&poll_command, child.actor.identity(), inbox_key, &foreign),
        Err(exomonad_actor::NotificationError::Unauthorized)
    );
    let stale = ActorRef {
        incarnation: exomonad_actor::Incarnation(child.actor.identity().incarnation.0 + 1),
        ..child.actor.identity()
    };
    assert_eq!(
        observe_notification_receipt(&poll_command, stale, inbox_key, &inbox),
        Err(exomonad_actor::NotificationError::InvalidReceipt)
    );
    drop(inbox);
    let inbox = ActorInbox::open(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path()).unwrap(),
        "rows-tree/deep/rows",
        "checkpoint-tree/deep/cursor",
    )
    .unwrap();
    assert_eq!(
        observe_notification_receipt(&poll_command, child.actor.identity(), inbox_key, &inbox),
        Ok(exomonad_actor::NotificationState::Accepted)
    );
    // Submitted transport acceptance is explicitly NOT model presentation.
    inbox
        .begin_tracked_delivery(poll_command.receipt().sequence())
        .unwrap()
        .submitted()
        .unwrap();
    assert_eq!(
        observe_notification_receipt(&poll_command, child.actor.identity(), inbox_key, &inbox),
        Ok(exomonad_actor::NotificationState::Unconfirmed)
    );
    poll_command.observed(result);
    let observed = campaign
        .drive_actor_output(&output_store, poll)
        .await
        .unwrap();
    assert_eq!(observed["status"], "committed", "{observed:?}");
    assert!(
        observed.to_string().contains("NotificationAccepted"),
        "{observed:?}"
    );
    let unchanged = campaign
        .drive_actor_output(
            &output_store,
            dispatch_haskell_script(child.policy.as_ref(), "display sessionInput"),
        )
        .await;
    assert_eq!(unchanged["status"], "committed", "{unchanged:?}");
    assert!(
        unchanged.to_string().contains("original assignment"),
        "{unchanged:?}"
    );
    campaign.assert_no_deployment("notification created an assignment/wake obligation", |_| {
        true
    });
    let reply =
        dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput :: Text)").await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    let answer = campaign
        .drive_actor_output(
            &output_store,
            dispatch_haskell_script(root.as_ref(), "_ <- pollResponse answer >>= display . show"),
        )
        .await;
    assert_eq!(answer["status"], "committed", "{answer:?}");
    assert!(
        answer.to_string().contains("original assignment"),
        "{answer:?}"
    );
    let owner_actor = campaign.root_installation.actor.identity();
    campaign
        .next_deployment(
            "owner settlement notification",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SettlementChanged { notification } => {
                    assert_eq!(notification.owner, owner_actor);
                    assert_eq!(notification.label, "notification-original");
                    assert_eq!(
                        notification.transition,
                        exomonad_actor::SettlementTransition::Ready
                    );
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let root_reply = dispatch_lookup(root.as_ref(), &["respond"]).await;
    assert!(
        root_reply.to_string().to_lowercase().contains("no match"),
        "{root_reply:?}"
    );
    let idle_setup = dispatch_haskell_script(
        root.as_ref(),
        "import qualified Tidepool.Agent.Contract as A\nRight idle <- spawnSubagent (FreshCtx \"idle notification recipient\") SameDir (defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))",
    )
    .await;
    assert_eq!(idle_setup["status"], "committed", "{idle_setup:?}");
    let idle = campaign
        .next_deployment(
            "never-assigned recipient policy",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    let policy = root.clone();
    let idle_send = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            "Right idleReceipt <- sendMessage idle \"idle notice\"",
        )
        .await
    });
    let command = campaign
        .next_deployment(
            "idle notification send",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationSend(command) => Ok(command),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(command.target(), idle.actor.identity());
    let idle_directory = tempfile::tempdir().unwrap();
    let idle_inbox = ActorInbox::open(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(idle_directory.path()).unwrap(),
        "rows-tree/deep/rows",
        "checkpoint-tree/deep/cursor",
    )
    .unwrap();
    let row = idle_inbox
        .publish_tracked(
            DurableActorEvent::Text(command.message().into()),
            DeliveryProvenance::Notification {
                sender: command.owner(),
                target: command.target(),
            },
        )
        .unwrap();
    command.admitted("idle-inbox".into(), row.sequence);
    let admitted = idle_send.await.unwrap();
    assert_eq!(admitted["status"], "committed", "{admitted:?}");
    for name in ["respond", "sessionReply", "sessionInput"] {
        let absent = dispatch_lookup(idle.policy.as_ref(), &[name]).await;
        assert!(
            absent.to_string().to_lowercase().contains("no match"),
            "idle recipient gained {name}: {absent:?}"
        );
    }
    campaign.assert_no_deployment("idle admission fabricated an activation", |_| true);
})).await;
}

/// Evidence from run 8a782b2b: while a typed request was pending for an
/// actor, a native delivery to it stayed in "actor inbox delivery remains
/// pending ... native operation" for several minutes, and in exactly that
/// window the actor's cells got "Variable not in scope: respond". The
/// durable delivery phase held here at `Accepted` — never advanced to
/// `Submitted`/`Presented` — reproduces the hold; `respond` must still
/// resolve the request while it stands.
#[tokio::test]
async fn held_native_delivery_preserves_typed_request_bindings() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let output_store =
                    display_output::open_run_store(campaign.session_root.path()).unwrap();
                let setup = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/notification_setup.hs",
                    ),
                )
                .await;
                assert_eq!(setup["status"], "committed", "{setup:?}");
                let child = campaign
                    .next_deployment("recipient policy", Duration::from_secs(120), |event| {
                        match event {
                            LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                            other => Err(other),
                        }
                    })
                    .await;
                let activation = campaign
                    .next_deployment(
                        "original request activation",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::SessionReady { activation } => Ok(activation),
                            other => Err(other),
                        },
                    )
                    .await;
                assert_eq!(activation.id.actor(), child.actor.identity());
                let directory = tempfile::tempdir().unwrap();
                let inbox = ActorInbox::open(
                    &tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path())
                        .unwrap(),
                    "rows-tree/deep/rows",
                    "checkpoint-tree/deep/cursor",
                )
                .unwrap();
                let inbox_key = "held-delivery-inbox";
                let policy = root.clone();
                let send = tokio::spawn(async move {
                    dispatch_haskell_script(
                        policy.as_ref(),
                        "Right receipt <- sendMessage worker \"one-way notice\"",
                    )
                    .await
                });
                let command = campaign
                    .next_deployment("notification send", Duration::from_secs(120), |event| {
                        match event {
                            LocalResidentDeployment::NotificationSend(command) => Ok(command),
                            other => Err(other),
                        }
                    })
                    .await;
                assert_eq!(command.target(), child.actor.identity());
                // Admit the row but never advance it past `Accepted` — no poll, no
                // `begin_tracked_delivery`, no `submitted()`/`presented()`. The row sits
                // exactly where a stuck native submission would leave it.
                admit_notification(&command, inbox_key.into(), &inbox);
                let sent = send.await.unwrap();
                assert_eq!(sent["status"], "committed", "{sent:?}");
                campaign.assert_no_deployment(
                    "a held delivery must not itself create an assignment/wake obligation",
                    |_| true,
                );

                // The typed request `child` is holding ("original assignment") is
                // untouched by the held delivery: `respond` still resolves it.
                let reply = dispatch_haskell_script(
                    child.policy.as_ref(),
                    "respond (sessionInput :: Text)",
                )
                .await;
                assert_eq!(reply["status"], "replied", "{reply:?}");
                let answer = campaign
                    .drive_actor_output(
                        &output_store,
                        dispatch_haskell_script(
                            root.as_ref(),
                            "_ <- pollResponse answer >>= display . show",
                        ),
                    )
                    .await;
                assert_eq!(answer["status"], "committed", "{answer:?}");
                assert!(
                    answer.to_string().contains("original assignment"),
                    "{answer:?}"
                );
            })
        })
        .await;
}

/// Companion to [`held_native_delivery_preserves_typed_request_bindings`]:
/// `lookup` selects the same request-aware workbench `respond` cells do,
/// and the extractor resolves the preamble's own declarations (where
/// `respond`/`sessionReply`/`sessionInput` are bound) through the query
/// module's typechecked environment, since that module is never loaded and
/// `getInfo` alone cannot see its top level.
#[tokio::test]
async fn lookup_during_held_native_delivery_returns_respond_signature() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let setup = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/notification_setup.hs",
                    ),
                )
                .await;
                assert_eq!(setup["status"], "committed", "{setup:?}");
                let child = campaign
                    .next_deployment("recipient policy", Duration::from_secs(120), |event| {
                        match event {
                            LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                            other => Err(other),
                        }
                    })
                    .await;
                campaign
                    .next_deployment(
                        "original request activation",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::SessionReady { activation } => Ok(activation),
                            other => Err(other),
                        },
                    )
                    .await;
                let directory = tempfile::tempdir().unwrap();
                let inbox = ActorInbox::open(
                    &tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path())
                        .unwrap(),
                    "rows-tree/deep/rows",
                    "checkpoint-tree/deep/cursor",
                )
                .unwrap();
                let inbox_key = "lookup-held-delivery-inbox";
                let policy = root.clone();
                let send = tokio::spawn(async move {
                    dispatch_haskell_script(
                        policy.as_ref(),
                        "Right receipt <- sendMessage worker \"one-way notice\"",
                    )
                    .await
                });
                let command = campaign
                    .next_deployment("notification send", Duration::from_secs(120), |event| {
                        match event {
                            LocalResidentDeployment::NotificationSend(command) => Ok(command),
                            other => Err(other),
                        }
                    })
                    .await;
                admit_notification(&command, inbox_key.into(), &inbox);
                let sent = send.await.unwrap();
                assert_eq!(sent["status"], "committed", "{sent:?}");

                for name in ["respond", "sessionReply", "sessionInput"] {
                    let found = dispatch_lookup(child.policy.as_ref(), &[name]).await;
                    assert!(
                        !found.to_string().to_lowercase().contains("no match"),
                        "expected {name} to resolve while the request is pending: {found:?}"
                    );
                }

                // Settle the request so the campaign shuts down cleanly.
                let reply = dispatch_haskell_script(
                    child.policy.as_ref(),
                    "respond (sessionInput :: Text)",
                )
                .await;
                assert_eq!(reply["status"], "replied", "{reply:?}");
            })
        })
        .await;
}

/// A structured reply within the notice budget arrives whole: the notice is
/// not the 512-character observation prefix a cell shows, so the owner has
/// the child's result without asking for it again.
#[tokio::test]
async fn settlement_notice_carries_a_structured_reply_whole_within_budget() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        "import qualified Tidepool.Agent.Contract as A\n\
         Right worker <- spawnSubagent (FreshCtx \"reply-whole-recipient\") SameDir (defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))\n\
         let requestName = \"reply-whole\" :: Text\n\
         Right answer <- request @[Text] worker [\"reply line \" <> tshow n | n <- [10 .. 99 :: Int]] (defaultRequestOptions { requestLabel = Just requestName })",
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let child = campaign
        .next_deployment(
            "recipient policy",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "original request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let reply =
        dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput :: [Text])").await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    campaign
        .next_deployment(
            "owner settlement notification carries the whole reply",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SettlementChanged { notification } => {
                    let preview = notification.reply_preview.as_deref().unwrap_or_default();
                    assert!(preview.len() > 512, "{preview}");
                    assert!(preview.starts_with("[reply line 10,\n"), "{preview}");
                    assert!(preview.ends_with("reply line 99]"), "{preview}");
                    assert!(!preview.contains("notice budget"), "{preview}");
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
})).await;
}

#[tokio::test]
async fn record_actor_dispatches_typed_routes_and_commits_state() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let result = dispatch_haskell_script(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/record_actor.hs",
                    ),
                )
                .await;
                assert_eq!(result["status"], "committed", "{result:?}");
                for item in result["items"].as_array().unwrap() {
                    assert_eq!(item["status"], "committed", "{result:?}");
                }
                assert!(result.to_string().contains("True"), "{result:?}");
            })
        })
        .await;
}

#[tokio::test]
async fn record_try_send_reports_mailbox_admission_without_waiting_for_handler() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(120),
                    dispatch_haskell_script(
                        campaign.root_installation.policy.as_ref(),
                        &tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/record_actor_try_send.hs",
                        ),
                    ),
                )
                .await
                .expect("self admission must not wait on the receiver's handler");
                assert_eq!(result["status"], "committed", "{result:?}");
                assert_eq!(
                    result["items"].as_array().unwrap().last().unwrap()["output"],
                    "True",
                    "{result:?}"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn record_actor_sleep_keeps_mailbox_handlers_sequential() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let result = dispatch_haskell_script(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/record_actor_sleep.hs",
                    ),
                )
                .await;
                assert_eq!(result["status"], "committed", "{result:?}");
                for item in result["items"].as_array().unwrap() {
                    assert_eq!(item["status"], "committed", "{result:?}");
                }
                assert!(result.to_string().contains("True"), "{result:?}");
            })
        })
        .await;
}

#[tokio::test]
async fn record_actor_nested_failure_reaches_interactive_owner_once() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let setup = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/record_actor_nested_failure.hs",
                    ),
                )
                .await;
                assert_eq!(setup["status"], "committed", "{setup:?}");
                let notice = campaign
                    .next_deployment(
                        "nested-handler failure notice",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::NotificationSend(command) => Ok(command),
                            other => Err(other),
                        },
                    )
                    .await;
                assert_eq!(notice.target(), campaign.actor.identity());
                assert!(notice.message().contains("nested-handler-probe"));
                let available = dispatch_haskell_script(
                    root.as_ref(),
                    "R.call (managerValue (R.client manager)) ()",
                )
                .await;
                assert_eq!(available["status"], "committed", "{available:?}");
                assert_eq!(available["items"][0]["output"], "7", "{available:?}");
                campaign.assert_no_deployment("duplicate failure notice", |_| true);
            })
        })
        .await;
}

#[tokio::test]
async fn record_actor_explains_invalid_state_shapes() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let setup = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/record_actor_invalid_shapes.hs",
                    ),
                )
                .await;
                assert_eq!(setup["status"], "committed", "{setup:?}");
                for source in [
        "invalid <- R.start (R.definition \"no-state\" Actor.ReadOnly (NoState (\\() -> pure ())))",
        "invalid <- R.start (R.definition \"two-states\" Actor.ReadOnly (TwoStates 0 False))",
    ] {
        let result = dispatch_haskell_script_result(root.as_ref(), source).await;
        let diagnostic = match result {
            Ok(value) => value.to_string(),
            Err(error) => error.to_string(),
        };
        assert!(
            diagnostic.contains("must declare exactly one State field"),
            "{diagnostic}"
        );
    }
            })
        })
        .await;
}

#[tokio::test]
async fn stateful_actor_drains_accepted_messages_into_its_retained_exit() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let result = dispatch_haskell_script(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/stateful_drain.hs",
                    ),
                )
                .await;
                assert_eq!(result["status"], "committed", "{result:?}");
                for item in result["items"].as_array().unwrap() {
                    assert_eq!(item["status"], "committed", "{result:?}");
                }
                assert!(result.to_string().contains("True"), "{result:?}");
                assert!(
                    !result.to_string().contains("preview unavailable"),
                    "{result:?}"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn stateful_handler_failure_after_effect_pauses_without_replay_or_closing_mailbox() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/stateful_failure.hs"),
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    for item in setup["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{setup:?}");
    }
    assert!(setup.to_string().contains("True"), "{setup:?}");
    let failed =
        dispatch_haskell_script(root.as_ref(), "cast server (Counter (-1) (const ()))").await;
    assert_eq!(failed["status"], "committed", "{failed:?}");
    assert!(
        !failed.to_string().contains("preview unavailable"),
        "{failed:?}"
    );
    let notice = campaign
        .next_deployment(
            "stateful-handler failure notice",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationSend(command) => Ok(command),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(notice.target(), campaign.actor.identity());
    assert!(
        notice.message().contains("handler-probe"),
        "{}",
        notice.message()
    );
    let rejected = dispatch_haskell_script_result(root.as_ref(), "drainActor server")
        .await
        .expect_err("paused drain must reject");
    assert!(
        rejected
            .to_string()
            .contains("replace a failed handler first"),
        "{rejected:?}"
    );
    let queued = dispatch_haskell_script(root.as_ref(), "cast server (Counter 5 (const ()))\npaused <- pollExit server\neffects <- call sink (Counter 0 id)\ncase paused of { Nothing -> effects == 1; _ -> False }").await;
    assert_eq!(queued["status"], "committed", "{queued:?}");
    for item in queued["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{queued:?}");
    }
    assert!(queued.to_string().contains("True"), "{queued:?}");
    assert!(
        !queued.to_string().contains("preview unavailable"),
        "{queued:?}"
    );
    campaign.assert_no_deployment("duplicate failure notice", |_| true);
    let repaired = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/stateful_replacement.hs"),
    )
    .await;
    assert_eq!(repaired["status"], "committed", "{repaired:?}");
    for item in repaired["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{repaired:?}");
    }
    assert!(!repaired.to_string().contains("False"), "{repaired:?}");
})).await;
}

#[tokio::test]
async fn lifecycle_sources_follow_replacement_and_capture_retained_exit() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                for (stage, source) in tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/lifecycle_source.hs",
                )
                .split("-- STAGE --\n")
                .enumerate()
                {
                    eprintln!("lifecycle fixture stage {stage} starting");
                    let root = campaign.root_installation.policy.clone();
                    let run = dispatch_haskell_script(root.as_ref(), source);
                    tokio::pin!(run);
                    let result = tokio::time::timeout(Duration::from_secs(180), async {
            tokio::select! {
                result = &mut run => result,
                command = campaign.next_deployment(
                    "lifecycle notification",
                    Duration::from_secs(180),
                    |event| match event {
                        LocalResidentDeployment::NotificationSend(command) => Ok(command),
                        other => Err(other),
                    },
                ) => {
                    panic!("stage {stage}: {}", command.message());
                }
            }
        })
        .await
        .expect("lifecycle fixture stage did not settle");
                    assert_eq!(result["status"], "committed", "stage {stage}: {result:?}");
                    for item in result["items"].as_array().unwrap() {
                        assert_eq!(item["status"], "committed", "stage {stage}: {result:?}");
                    }
                    assert!(
                        !result.to_string().contains("False"),
                        "stage {stage}: {result:?}"
                    );
                    eprintln!("lifecycle fixture stage {stage} completed");
                }
            })
        })
        .await;
}

#[tokio::test]
async fn stateful_replacement_rejects_changed_state_and_protocol_types() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let imports =
        dispatch_haskell_script(root.as_ref(), "import Tidepool.Actor hiding (Source)").await;
    assert_eq!(imports["status"], "committed", "{imports:?}");
    for (script, expected_types) in [
        (
            "invalidReplacement <- replaceActor (undefined :: ActorRef ((,) Int) Int) (stateful \"wrong-state\" ReadOnly (\\state (_, reply) -> pure (reply, state)) :: ActorDefinition Bool ((,) Int) Bool)",
            ["Int", "Bool"],
        ),
        (
            "invalidReplacement <- replaceActor (undefined :: ActorRef ((,) Int) Int) (stateful \"wrong-protocol\" ReadOnly (\\state (_, reply) -> pure (reply, state)) :: ActorDefinition Int ((,) Bool) Int)",
            ["Int", "Bool"],
        ),
    ] {
        let result = dispatch_haskell_script_result(root.as_ref(), script).await;
        let diagnostic = match result {
            Ok(value) => value.to_string(),
            Err(error) => error.to_string(),
        };
        assert!(diagnostic.contains("Couldn't match"), "{diagnostic}");
        for ty in expected_types {
            assert!(diagnostic.contains(ty), "{diagnostic}");
        }
        assert!(
            !diagnostic.contains("Variable not in scope"),
            "{diagnostic}"
        );
    }
})).await;
}

#[tokio::test]
async fn stateful_replacement_preserves_owned_children() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let ownership = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/stateful_replacement_tree.hs",
                    ),
                )
                .await;
                assert_eq!(ownership["status"], "committed", "{ownership:?}");
                for item in ownership["items"].as_array().unwrap() {
                    assert_eq!(item["status"], "committed", "{ownership:?}");
                }
                assert!(
                    ownership.to_string().contains("True")
                        && !ownership.to_string().contains("False"),
                    "{ownership:?}"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn haskell_mailbox_preserves_state_and_opaque_replies_across_calls() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let result = dispatch_haskell_script(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/mailbox_state.hs",
                    ),
                )
                .await;
                assert_eq!(result["status"], "committed", "{result:?}");
                for item in result["items"].as_array().unwrap() {
                    assert_eq!(item["status"], "committed", "{result:?}");
                }
                assert!(result.to_string().contains("True"), "{result:?}");
            })
        })
        .await;
}

#[test]
fn worker_workspaces_are_distinct_linked_worktrees_in_one_git_namespace() {
    let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("README.md", "source\n", "seed")
        .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let (manager, _bindings) = actor_worktree_resources_at(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(storage.path()).unwrap(),
        repository.path(),
    )
    .unwrap();
    let first = manager
        .create(&WorktreeSpec::from_current_repository("first-worker"))
        .unwrap();
    let second = manager
        .create(&WorktreeSpec::from_current_repository("second-worker"))
        .unwrap();

    assert_ne!(first.id(), second.id());
    assert_ne!(first.cwd(), second.cwd());
    assert!(!first.cwd().starts_with(repository.path()));
    assert!(!second.cwd().starts_with(repository.path()));
    assert!(first.cwd().join(".git").is_file());
    assert!(second.cwd().join(".git").is_file());
    assert_eq!(
        exomonad_worktree::git::inspect::git_common_dir(manager.git(), first.cwd()).unwrap(),
        repository.path().join(".git")
    );
    assert_eq!(
        exomonad_worktree::git::inspect::git_common_dir(manager.git(), second.cwd()).unwrap(),
        repository.path().join(".git")
    );
    assert_eq!(
        std::fs::read_to_string(repository.path().join("README.md")).unwrap(),
        "source\n"
    );
}

fn delivery_phase(inbox: &ActorInbox, sequence: u64) -> exomonad_node::DeliveryPhase {
    match inbox.observe_receipt(sequence).unwrap() {
        exomonad_node::ReceiptLookup::Retained(evidence) => evidence.phase,
        exomonad_node::ReceiptLookup::Unavailable => panic!("receipt {sequence} unavailable"),
    }
}

#[tokio::test]
async fn composition_root_child_session_factory_runs_a_cell() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let child_session_id = tidepool_runtime::session::fresh_session_id();
                let mut child_machine = (campaign.child_session_factory)(child_session_id, &[])
                    .expect("the composition root's factory builds a fresh session");

                child_machine.set_effect_execution(
                    EffectRunPolicy::HandleOrSuspend,
                    LivePayloadPolicy::HASKELL_EFFECT_VALUE,
                );
                let lexical_scope = child_machine.mint_isolated_scope();
                child_machine
                    .set_actor_execution(
                        tidepool_runtime::session::SessionRunContext {
                            lexical_scope,
                            resource_scope: tidepool_codegen::suspension::RealmId::fresh(),
                            ..tidepool_runtime::session::SessionRunContext::ROOT
                        },
                        EffectRunPolicy::HandleOrSuspend,
                        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
                    )
                    .expect("a freshly bootstrapped machine accepts actor execution context");
                let outcome = child_machine
        .run_with_sites("exomonad_root_driver", campaign.program.code())
        .expect("the driver cell the root itself bootstraps with also runs on a child machine");
                assert!(
        matches!(
            outcome,
            tidepool_runtime::session::ResidentOutcome::Suspended { .. }
        ),
        "expected the driver cell to suspend attaching its permanent application, got {outcome:?}"
    );
            })
        })
        .await;
}

#[tokio::test]
async fn descendants_list_the_spawn_tree_and_drop_a_retired_leaf() {
    // Two fork levels: the child can launch one further generation, which
    // then has no descendant budget left.
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let root_id = campaign.actor.identity();

                let root_for_setup = root.clone();
                let setup = tokio::spawn(async move {
                    dispatch_haskell_script(
                        root_for_setup.as_ref(),
                        &tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/descendants_setup.hs",
                        ),
                    )
                    .await
                });
                let child_installation = campaign
                    .next_deployment(
                        "descendants child policy installation",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::PolicyInstalled(installation) => {
                                Ok(installation)
                            }
                            other => Err(other),
                        },
                    )
                    .await;
                let child_id = child_installation.actor.identity();
                campaign.authority.install_grant(
                    child_id.into(),
                    ActorWorktreeGrant::Bound {
                        enumerate: false,
                        allocate: true,
                        integrate: true,
                    },
                );
                campaign.acknowledge_native_spawn(&child_installation);
                let setup = setup.await.unwrap();
                assert_eq!(setup["status"], "committed", "{setup:?}");

                let child_policy = child_installation.policy.clone();
                let child_spawn = tokio::spawn(async move {
                    dispatch_haskell_script(
                        child_policy.as_ref(),
                        &tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/descendants_child_spawn.hs",
                        ),
                    )
                    .await
                });
                let grandchild_installation = campaign
                    .next_deployment(
                        "descendants grandchild policy installation",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::PolicyInstalled(installation) => {
                                Ok(installation)
                            }
                            other => Err(other),
                        },
                    )
                    .await;
                campaign.authority.install_grant(
                    grandchild_installation.actor.identity().into(),
                    ActorWorktreeGrant::Bound {
                        enumerate: false,
                        allocate: true,
                        integrate: true,
                    },
                );
                campaign.acknowledge_native_spawn(&grandchild_installation);
                let child_spawn = child_spawn.await.unwrap();
                assert_eq!(child_spawn["status"], "committed", "{child_spawn:?}");

                let observed = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/descendants_observe.hs",
                    ),
                )
                .await;
                assert_eq!(observed["status"], "committed", "{observed:?}");
                let rendered = observed.to_string();
                assert!(
        rendered.contains("descendants-child") && rendered.contains("descendants-grandchild"),
        "expected both the child and the grandchild in the root's descendants: {rendered}"
    );
                assert!(
        rendered.contains(&format!("rosterCreatorId = Just {}", child_id.id.0)),
        "expected the grandchild's roster entry to name the child as its creator: {rendered}"
    );
                assert!(
                    rendered.contains(&format!("rosterCreatorId = Just {}", root_id.id.0)),
                    "expected the child's roster entry to name the root as its creator: {rendered}"
                );

                let stopped = dispatch_haskell_script(
                    child_installation.policy.as_ref(),
                    "stopAgent (responseActor grandchildResponse)",
                )
                .await;
                assert_eq!(stopped["status"], "committed", "{stopped:?}");

                let after_retirement = dispatch_haskell_script(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/descendants_observe.hs",
                    ),
                )
                .await;
                assert_eq!(
                    after_retirement["status"], "committed",
                    "{after_retirement:?}"
                );
                let rendered_after = after_retirement.to_string();
                assert!(
                    rendered_after.contains("descendants-child"),
                    "the live child must remain: {rendered_after}"
                );
                assert!(
        !rendered_after.contains("descendants-grandchild"),
        "the retired grandchild must drop out of the caller's live descendants: {rendered_after}"
    );
            })
        })
        .await;
}

#[tokio::test]
async fn forest_operator_survives_model_root_recovery() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let operator = campaign
                    .forest
                    .new_workbench(
                        "operator".into(),
                        exomonad_actor::ActorCapabilities::default(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    campaign
                        .forest
                        .inspect_graph(campaign.actor.identity())
                        .unwrap()
                        .len(),
                    1,
                    "ordinary roots cannot inspect other trees"
                );
                assert_eq!(
                    campaign
                        .forest
                        .inspect_graph(operator.identity())
                        .unwrap()
                        .len(),
                    2
                );
                async fn submit(
                    actor: &LocalActorRef,
                    source: &str,
                ) -> tidepool_runtime::session::WorkbenchResponse {
                    let (reply, receive) = tokio::sync::oneshot::channel();
                    actor
                        .address()
                        .send_message(exomonad_actor::KernelMessage::Workbench {
                            invocation: exomonad_actor::ActorWorkbenchInvocation::unbound(
                                tidepool_runtime::session::WorkbenchRequest::from_cell_input(
                                    source,
                                ),
                            ),
                            control: None,
                            reply: reply.into(),
                        })
                        .unwrap();
                    receive.await.unwrap().unwrap()
                }
                let bound = submit(&operator, "let retainedOperatorValue = 123").await;
                assert_eq!(
                    bound.status,
                    tidepool_runtime::session::WorkbenchRunStatus::Committed
                );
                let requested = submit(
                    &operator,
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/operator_request.hs",
                    ),
                )
                .await;
                assert_eq!(
                    requested.status,
                    tidepool_runtime::session::WorkbenchRunStatus::Committed,
                    "{requested:?}"
                );
                let child = campaign
                    .next_deployment(
                        "operator child admission",
                        Duration::from_secs(30),
                        |event| match event {
                            LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                            other => Err(other),
                        },
                    )
                    .await;
                assert_eq!(child.supervisor_parent, Some(operator.identity()));
                assert_eq!(child.context_parent, None);
                assert_eq!(
                    campaign
                        .forest
                        .inspect_graph(child.actor.identity())
                        .unwrap()
                        .len(),
                    1,
                    "operator forest grant must not propagate to descendants"
                );
                let replied = dispatch_haskell_script(
                    child.policy.as_ref(),
                    "respond (sessionInput + 1 :: Int)",
                )
                .await;
                assert_eq!(replied["status"], "replied", "{replied:?}");
                let response = submit(&operator, "inspectFull <$> pollResponse answer").await;
                assert!(
                    response.items.iter().any(|item| item.output.contains("42")),
                    "{response:?}"
                );
                campaign
                    .actor
                    .shutdown(ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: "recovery test".into(),
                        diagnostic: None,
                    })
                    .await
                    .unwrap();
                campaign.observe_hosted_completion().await.unwrap();
                assert_eq!(
                    campaign.forest.resident_session_state(),
                    tidepool_runtime::session::ResidentSessionState::Reusable,
                    "actor failure must not imply that the resident machine is safe to replace"
                );
                let (replacement, task) = campaign
                    .forest
                    .recover_program_root(
                        campaign.actor.identity(),
                        "replacement".into(),
                        exomonad_actor::ActorCapabilities::default(),
                        campaign.program.clone(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    campaign.forest.resident_session_state(),
                    tidepool_runtime::session::ResidentSessionState::Reusable
                );
                assert_ne!(replacement.identity(), campaign.actor.identity());
                assert_eq!(
                    submit(&operator, "retainedOperatorValue").await.items[0].output,
                    "123"
                );
                assert_eq!(
                    campaign
                        .forest
                        .inspect_graph(replacement.identity())
                        .unwrap()
                        .len(),
                    1
                );
                assert!(campaign
                    .forest
                    .inspect_graph(operator.identity())
                    .unwrap()
                    .iter()
                    .any(|node| node.actor == replacement.identity()));
                campaign.observe_forest_shutdown().await;
                task.await.unwrap();
                assert!(operator.terminal().get().is_some());
            })
        })
        .await;
}

#[tokio::test]
async fn request_update_keeps_original_request_and_fences_terminal_delivery() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/request_update_setup.hs"),
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let child = campaign
        .next_deployment(
            "request-update child policy installation",
            Duration::from_secs(30),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "request-update session readiness",
            Duration::from_secs(30),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let failed = dispatch_haskell_script(
        root.as_ref(),
        "Right failedClarification <- updateRequest answer \"Private baseline clarification\"",
    )
    .await;
    assert_eq!(failed["status"], "committed", "{failed:?}");
    let failed_delivery = campaign
        .next_deployment(
            "failed clarification request update",
            Duration::from_secs(30),
            |event| match event {
                LocalResidentDeployment::RequestUpdate { delivery } => Ok(delivery),
                LocalResidentDeployment::SessionReady { .. } => {
                    panic!("update queued another assignment")
                }
                other => Err(other),
            },
        )
        .await;
    let presentation = failed_delivery.begin().unwrap();
    let key = presentation.key().to_owned();
    let log = tempfile::NamedTempFile::new().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log.reopen().unwrap()))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        presentation.not_presented(
            "native input was not submitted: connecting update proxy: controlled transport failure"
                .into(),
        )
    });
    let logged = std::fs::read_to_string(log.path()).unwrap();
    for expected in [
        "request update not presented",
        "actor=ActorRef",
        "request=RequestId",
        "update=1",
        &key,
        "connecting update proxy",
    ] {
        assert!(logged.contains(expected), "missing {expected}: {logged}");
    }
    assert!(!logged.contains("Private baseline clarification"));
    let failed_state =
        dispatch_haskell_script(root.as_ref(), "pollRequestUpdate failedClarification").await;
    assert!(
        failed_state.to_string().contains("UpdateNotPresented"),
        "{failed_state:?}"
    );
    assert!(
        !failed_state.to_string().contains("agent run failed"),
        "{failed_state:?}"
    );
    let pending = dispatch_haskell_script(root.as_ref(), "pollResponse answer").await;
    assert!(
        pending.to_string().contains("ResponsePending"),
        "{pending:?}"
    );
    // Carries the target's own progress (lifecycle, provider health, last
    // activity, progress revision) as data, so a caller has something to
    // look at besides "pending" again.
    assert!(pending.to_string().contains("state="), "{pending:?}");
    let sent = dispatch_haskell_script(
        root.as_ref(),
        "Right clarification <- updateRequest answer \"Tabs must be clickable\"",
    )
    .await;
    assert_eq!(sent["status"], "committed", "{sent:?}");
    let queued = dispatch_haskell_script(root.as_ref(), "pollRequestUpdate clarification").await;
    assert!(
        queued.to_string().contains("Right UpdateQueued"),
        "{queued:?}"
    );
    let delivery = campaign
        .next_deployment(
            "clarification request update",
            Duration::from_secs(30),
            |event| match event {
                LocalResidentDeployment::RequestUpdate { delivery } => Ok(delivery),
                LocalResidentDeployment::SessionReady { .. } => {
                    panic!("update queued another assignment")
                }
                other => Err(other),
            },
        )
        .await;
    let presentation = delivery.begin().unwrap();
    assert!(presentation.message().contains("Tabs must be clickable"));
    let rejected = dispatch_haskell_script(
        child.policy.as_ref(),
        "import Tidepool.Agent.Reply (attemptReply)\nattemptReply sessionReply (sessionInput + 32)",
    )
    .await;
    assert!(
        rejected.to_string().contains("ReplyUpdatePending"),
        "{rejected:?}"
    );
    let pending = dispatch_haskell_script(root.as_ref(), "pollResponse answer").await;
    assert!(
        pending.to_string().contains("ResponsePending"),
        "{pending:?}"
    );
    assert!(pending.to_string().contains("state="), "{pending:?}");
    // The backend seam owns the proof of input insertion. This test drives
    // that boundary explicitly, without sending input to a live model.
    presentation.presented();
    let observed = dispatch_haskell_script(root.as_ref(), "pollRequestUpdate clarification").await;
    assert!(
        observed.to_string().contains("Right UpdatePresented"),
        "{observed:?}"
    );
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput + 32)").await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    let ready = dispatch_haskell_script(root.as_ref(), "pollResponse answer >>= \\s -> pure (case s of { ResponseReady result -> responseValue result == 42; _ -> False })").await;
    assert_eq!(ready["items"][0]["output"], "True", "{ready:?}");
    // A correction sent after the child has already replied is refused by
    // the send itself. It used to be accepted, leaving the caller to learn
    // from a second observation that nobody would ever see it — which is
    // too late to steer anything.
    let late = dispatch_haskell_script(root.as_ref(), "updateRequest answer \"too late\"").await;
    assert!(
        late.to_string().contains("Left ReplyAlreadySettled"),
        "{late:?}"
    );
    campaign
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "active update test complete".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    campaign.observe_hosted_completion().await.unwrap();

    })).await;
}

#[tokio::test]
async fn settlement_notice_carries_a_readable_reply_preview() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        "import qualified Tidepool.Agent.Contract as A\n\
         Right worker <- spawnSubagent (FreshCtx \"reply-preview-recipient\") SameDir (defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))\n\
         let requestName = \"reply-preview\" :: Text\n\
         Right answer <- request @String worker (\"a readable reply\" :: String) (defaultRequestOptions { requestLabel = Just requestName })",
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let child = campaign
        .next_deployment(
            "recipient policy",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "original request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let reply =
        dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput :: String)").await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    let owner_actor = campaign.root_installation.actor.identity();
    campaign
        .next_deployment(
            "owner settlement notification carries the reply text",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SettlementChanged { notification } => {
                    assert_eq!(notification.owner, owner_actor);
                    assert_eq!(
                        notification.transition,
                        exomonad_actor::SettlementTransition::Ready
                    );
                    assert!(
                        notification
                            .reply_preview
                            .as_deref()
                            .is_some_and(|preview| preview.contains("a readable reply")),
                        "{:?}",
                        notification.reply_preview
                    );
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
})).await;
}

#[tokio::test]
async fn settlement_notice_carries_a_readable_reply_preview_for_text() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        "import qualified Tidepool.Agent.Contract as A\n\
         Right worker <- spawnSubagent (FreshCtx \"reply-preview-text-recipient\") SameDir (defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))\n\
         let requestName = \"reply-preview-text\" :: Text\n\
         Right answer <- request @Text worker (\"a readable reply\" :: Text) (defaultRequestOptions { requestLabel = Just requestName })",
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let child = campaign
        .next_deployment(
            "recipient policy",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "original request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let reply =
        dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput :: Text)").await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    let owner_actor = campaign.root_installation.actor.identity();
    campaign
        .next_deployment(
            "owner settlement notification carries the reply text",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SettlementChanged { notification } => {
                    assert_eq!(notification.owner, owner_actor);
                    assert_eq!(
                        notification.transition,
                        exomonad_actor::SettlementTransition::Ready
                    );
                    assert!(
                        notification
                            .reply_preview
                            .as_deref()
                            .is_some_and(|preview| preview.contains("a readable reply")),
                        "{:?}",
                        notification.reply_preview
                    );
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
})).await;
}

#[tokio::test]
async fn root_journal_effect_appends_a_typed_record() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let result = dispatch_haskell_script(
                    campaign.root_installation.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/journal_record.hs",
                    ),
                )
                .await;
                assert_eq!(result["status"], "committed", "{result:?}");
                let run_id = campaign
                    .session_root
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("model-free run root has a UTF-8 test id");
                let journal_path =
                    crate::exomonad::exomonad_journal_path(campaign._repository.path(), run_id);
                let lines: Vec<serde_json::Value> = std::fs::read_to_string(&journal_path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", journal_path.display()))
                    .lines()
                    .map(|line| serde_json::from_str(line).expect("journal line is JSON"))
                    .collect();
                assert_eq!(
                    lines.len(),
                    2,
                    "one version header and one record: {lines:?}"
                );
                assert!(lines[0].get("version").is_some(), "header: {lines:?}");
                assert_eq!(lines[1]["kind"], "test-kind");
                assert_eq!(lines[1]["key"], "test-key");
                assert_eq!(lines[1]["payload"], "payload");
            })
        })
        .await;
}

#[tokio::test]
async fn haskell_actor_sends_normal_steering_without_a_native_session() {
    let campaign = test_campaign::TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let policy = root.clone();
                let mut run = tokio::spawn(async move {
                    dispatch_haskell_script(
                        policy.as_ref(),
                        &tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/message_actor.hs",
                        ),
                    )
                    .await
                });
                let mut launched = None;
                let command = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            tokio::select! {
                result = &mut run, if launched.is_none() => {
                    let result = result.unwrap();
                    assert_eq!(result["status"], "committed", "{result:?}");
                    for item in result["items"].as_array().unwrap() {
                        assert_eq!(item["status"], "committed", "{result:?}");
                    }
                    launched = Some(result);
                }
                command = campaign.next_deployment(
                    "haskell sender notification",
                    Duration::from_secs(120),
                    |event| match event {
                        LocalResidentDeployment::NotificationSend(command) => Ok(command),
                        LocalResidentDeployment::PolicyInstalled(_) => {
                            panic!("Haskell sender acquired a model session")
                        }
                        other => Err(other),
                    },
                ) => break command,
            }
        }
    })
    .await
    .unwrap();
                assert_ne!(command.owner(), campaign.actor.identity());
                assert_eq!(command.target(), campaign.actor.identity());
                assert_eq!(command.message(), "e434: retain candidate; check digest");
                let directory = tempfile::tempdir().unwrap();
                let inbox = ActorInbox::open(
                    &tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path())
                        .unwrap(),
                    "rows",
                    "cursor",
                )
                .unwrap();
                admit_notification(&command, "actor-message-inbox".into(), &inbox);
                let launched = match launched {
                    Some(result) => result,
                    None => run.await.unwrap(),
                };
                assert_eq!(launched["status"], "committed", "{launched:?}");
                let finished = dispatch_haskell_script(
        root.as_ref(),
        "finished <- awaitExit relay\ncase finished of { Completed (Right _) -> True; _ -> False }",
    )
    .await;
                assert_eq!(finished["status"], "committed", "{finished:?}");
                assert!(finished.to_string().contains("True"), "{finished:?}");
            })
        })
        .await;
}

#[test]
fn legacy_run_status_fences_only_its_workspace() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let run = runs.join("old-run");
    std::fs::create_dir_all(&run).unwrap();
    let workspace = root.path().join("project");
    std::fs::write(
        run.join("status.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 4, "run_id": "old-run", "workspace": workspace,
            "session": "old-session", "agent": {"model": "test-model", "effort": "low"},
            "phase": {"state": "starting"}
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        active_run_for_workspace_in(&runs, &workspace).unwrap(),
        Some(run)
    );
    assert_eq!(
        active_run_for_workspace_in(&runs, &root.path().join("other")).unwrap(),
        None
    );
}
