use super::display_output;
use super::test_campaign::{
    committed_display_text, dispatch_haskell_script, dispatch_haskell_script_with_deadline,
    TestCampaign, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
};
use exomonad_actor::{ActorExitKind, ActorTerminal, LocalResidentDeployment};
use std::time::Duration;

#[tokio::test]
async fn fresh_context_typed_exit_and_replacement_retain_callable_value() {
    let campaign = TestCampaign::start_with_child_sessions().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
        let root = campaign.root_installation.policy.clone();
        let root_id = campaign.actor.identity();
        let root_session = campaign.forest.actor_session(root_id).unwrap();
        let installed = dispatch_haskell_script(root.as_ref(),
            &tidepool_testing::fixture_source("bridge/facade/src/actor_host/fresh_callable_exit_setup.hs")).await;
        assert_eq!(installed["status"], "committed", "{installed}");
        let predecessor = campaign.forest.inspect_host_graph().into_iter()
            .find(|node| node.label == "fresh-callable-exit").unwrap();
        let predecessor_session = campaign.forest.actor_session(predecessor.actor).unwrap();
        assert_ne!(predecessor_session, root_session, "typed exit crosses actual machines");
        let replaced = dispatch_haskell_script(root.as_ref(),
            "callableExitSuccessor <- replaceActor callableExitActor callableExitDefinition").await;
        assert_eq!(replaced["status"], "committed", "{replaced}");
        let successor = campaign.forest.inspect_host_graph().into_iter()
            .find(|node| node.label == "fresh-callable-exit" && node.actor != predecessor.actor).unwrap();
        let successor_session = campaign.forest.actor_session(successor.actor).unwrap();
        assert_ne!(successor_session, root_session);
        assert_ne!(successor_session, predecessor_session, "replacement owns a fresh machine");
        let store = display_output::open_run_store(campaign.session_root.path()).unwrap();
        let previous = campaign.drive_actor_output(&store, dispatch_haskell_script(root.as_ref(),
            "previousExit <- awaitExit callableExitActor\ndisplay (case previousExit of { Cancelled _ -> True; _ -> False })")).await;
        assert_eq!(committed_display_text(&previous), "True", "{previous}");
        let drained = dispatch_haskell_script(root.as_ref(), "drainActor callableExitSuccessor").await;
        assert_eq!(drained["status"], "committed", "{drained}");
        campaign.next_deployment("callable successor retirement", Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::Retired { actor, terminal } if actor == successor.actor => Ok(terminal),
                other => Err(other),
            }).await;
        assert_eq!(campaign.forest.session_state_of(successor_session),
            tidepool_runtime::session::ResidentSessionState::Gone);
        let returned = campaign.drive_actor_output(&store, dispatch_haskell_script(root.as_ref(),
            &tidepool_testing::fixture_source("bridge/facade/src/actor_host/fresh_callable_exit_read.hs"))).await;
        assert_eq!(committed_display_text(&returned), "True", "{returned}");
    })).await;
}

#[tokio::test]
async fn fresh_context_source_reads_actual_progress_and_result_before_typed_exit() {
    let campaign = TestCampaign::start_with_child_sessions().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
        let root = campaign.root_installation.policy.clone();
        let root_id = campaign.actor.identity();
        let root_session = campaign.forest.actor_session(root_id).unwrap();
        let (setup, producer) = campaign.dispatch_native_spawn_setup(
            "source producer setup and native acknowledgement",
            &tidepool_testing::fixture_source("bridge/facade/src/actor_host/source_setup.hs"),
            COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
        ).await;
        assert_eq!(setup["status"], "committed", "{setup}");
        campaign.await_native_activation(producer.actor.identity(),
            "source producer typed activation", Duration::from_secs(120)).await;
        assert_eq!(campaign.forest.actor_session(producer.actor.identity()), Some(root_session),
            "captured-context producer retains its issuing machine");
        let first = dispatch_haskell_script_with_deadline(producer.policy.as_ref(),
            "reportProgress (ProgressNote 1 (+ sessionInput))", "source first progress", COLD_DEBUG_CELL_SETTLEMENT_BUDGET).await;
        assert_eq!(first["status"], "committed", "{first}");
        let installed = dispatch_haskell_script_with_deadline(root.as_ref(),
            &tidepool_testing::fixture_source("bridge/facade/src/actor_host/fresh_result_source_actor.hs"), "source collector installation", COLD_DEBUG_CELL_SETTLEMENT_BUDGET).await;
        assert_eq!(installed["status"], "committed", "{installed}");
        let collector = campaign.forest.inspect_host_graph().into_iter()
            .find(|node| node.label == "ordered-source-collector").unwrap();
        let collector_session = campaign.forest.actor_session(collector.actor).unwrap();
        assert_ne!(collector_session, root_session, "source observation crosses actual machines");
        let published = dispatch_haskell_script_with_deadline(producer.policy.as_ref(),
            "reportProgress (ProgressNote 2 (* sessionInput))\nreportProgress (ProgressNote 3 (subtract sessionInput))\nrespond (42 :: Int)", "source remaining progress and result", COLD_DEBUG_CELL_SETTLEMENT_BUDGET).await;
        assert_eq!(published["status"], "replied", "{published}");
        let settled = dispatch_haskell_script_with_deadline(root.as_ref(),
            "settled <- watch (Just \"fresh-source-result\") (result answer)", "source settlement watch", COLD_DEBUG_CELL_SETTLEMENT_BUDGET).await;
        assert_eq!(settled["status"], "committed", "{settled}");
        campaign.next_deployment("source settlement is ready", Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::WatchChanged { notification } if notification.owner == root_id && notification.label == "fresh-source-result" => {
                    assert_eq!(notification.transition, exomonad_actor::WatchTransition::Ready);
                    Ok(())
                }
                other => Err(other),
            }).await;
        let drained = dispatch_haskell_script_with_deadline(root.as_ref(), "drainActor collector", "source collector drain", COLD_DEBUG_CELL_SETTLEMENT_BUDGET).await;
        assert_eq!(drained["status"], "committed", "{drained}");
        campaign.next_deployment("source collector retirement", Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::Retired { actor, terminal } if actor == collector.actor => Ok(terminal),
                other => Err(other),
            }).await;
        assert_eq!(campaign.forest.session_state_of(collector_session),
            tidepool_runtime::session::ResidentSessionState::Gone);
        let store = display_output::open_run_store(campaign.session_root.path()).unwrap();
        let collected = campaign.drive_actor_output(&store, dispatch_haskell_script_with_deadline(root.as_ref(),
            "collected <- awaitExit collector\ndisplay (case collected of { Completed values -> reverse values == [13, 30, -7, -1, 42]; _ -> False })", "source callable values after collector Gone", COLD_DEBUG_CELL_SETTLEMENT_BUDGET)).await;
        assert_eq!(committed_display_text(&collected), "True", "{collected}");
    })).await;
}

#[tokio::test]
async fn native_spawn_setup_refuses_settlement_without_child_admission() {
    use futures_util::FutureExt;
    let campaign = TestCampaign::start_with_child_sessions().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let refusal = std::panic::AssertUnwindSafe(campaign.dispatch_native_spawn_setup(
                    "no-spawn setup control",
                    "let nativeSpawnSetupControl = (41 :: Int)",
                    COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                ))
                .catch_unwind()
                .await;
                let failure = refusal
                    .err()
                    .expect("setup without a spawn admission must fail");
                let message = failure
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| failure.downcast_ref::<&str>().copied())
                    .unwrap_or("");
                assert!(
                    message.contains("settled before native child installation"),
                    "{message}"
                );
                assert!(
                    message.contains("committed"),
                    "the actual settled receipt is retained: {message}"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn fresh_context_callable_reply_survives_retirement_and_request_forget() {
    callable_reply_scenario(
        "respond ((sessionInput + 1 :: Int), (\\n -> n + sessionInput))",
        true,
    )
    .await;
}

#[tokio::test]
async fn fresh_context_callable_oracle_rejects_wrong_closure_value() {
    callable_reply_scenario(
        "respond ((sessionInput + 1 :: Int), (\\n -> n + sessionInput + 1))",
        false,
    )
    .await;
}

async fn callable_reply_scenario(reply_source: &'static str, expected: bool) {
    let campaign = TestCampaign::start_with_child_sessions().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let root_session = campaign
                    .forest
                    .actor_session(campaign.actor.identity())
                    .unwrap();
                let (setup, child) = campaign
                    .dispatch_native_spawn_setup(
                        "callable child setup and native acknowledgement",
                        &tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/fresh_callable_result_setup.hs",
                        ),
                        COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                    )
                    .await;
                let child_id = child.actor.identity();
                let child_session = campaign.forest.actor_session(child_id).unwrap();
                assert_ne!(
                    child_session, root_session,
                    "callable response crosses actual machines"
                );
                assert_eq!(setup["status"], "committed", "{setup}");
                let root_id = campaign.actor.identity();
                campaign
                    .await_native_activation(
                        child_id,
                        "callable child typed activation",
                        Duration::from_secs(120),
                    )
                    .await;
                let replied = dispatch_haskell_script(child.policy.as_ref(), reply_source).await;
                assert_eq!(replied["status"], "replied", "{replied}");
                campaign
                    .next_deployment(
                        "callable watch captures successful settlement",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::WatchChanged { notification }
                                if notification.owner == root_id
                                    && notification.label == "fresh-callable-result" =>
                            {
                                assert_eq!(
                                    notification.transition,
                                    exomonad_actor::WatchTransition::Ready
                                );
                                Ok(())
                            }
                            other => Err(other),
                        },
                    )
                    .await;
                child
                    .actor
                    .shutdown(ActorTerminal {
                        kind: ActorExitKind::Completed,
                        summary: "callable result published".into(),
                        diagnostic: None,
                    })
                    .await
                    .unwrap();
                campaign
                    .next_deployment(
                        "callable child retirement",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::Retired { actor, terminal }
                                if actor == child_id =>
                            {
                                Ok(terminal)
                            }
                            other => Err(other),
                        },
                    )
                    .await;
                assert_eq!(
                    campaign.forest.session_state_of(child_session),
                    tidepool_runtime::session::ResidentSessionState::Gone
                );
                let store = display_output::open_run_store(campaign.session_root.path()).unwrap();
                let read = campaign
                    .drive_actor_output(
                        &store,
                        dispatch_haskell_script(
                            root.as_ref(),
                            &tidepool_testing::fixture_source(
                                "bridge/facade/src/actor_host/fresh_callable_result_read.hs",
                            ),
                        ),
                    )
                    .await;
                assert_eq!(
                    committed_display_text(&read),
                    if expected { "True" } else { "False" },
                    "{read}"
                );
                let released = campaign
                    .drive_actor_output(
                        &store,
                        dispatch_haskell_script(
                            root.as_ref(),
                            &tidepool_testing::fixture_source(
                                "bridge/facade/src/actor_host/fresh_callable_result_release.hs",
                            ),
                        ),
                    )
                    .await;
                assert_eq!(
                    committed_display_text(&released),
                    if expected { "True" } else { "False" },
                    "{released}"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn fresh_context_inherited_response_preserves_value_and_watch_custody() {
    super::custody_tests::inherited_response_scenario(
        TestCampaign::start_with_child_sessions().await,
        true,
    )
    .await;
}

/// A workspace nominal input must keep its original identity while the selected
/// child compiles its source closure under a narrower invocation effect row.
#[tokio::test(flavor = "multi_thread")]
async fn scaffolded_selected_coding_child_preserves_workspace_input_and_effect_row() {
    use super::hosted_test_context::HostedTestRuntime;
    use super::test_campaign::{
        hosted_script_provider, hosted_test_settings, next_hosted_script_round,
        require_ghc_compile_rejection, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
    };
    use exomonad_tool::ActorEffectKey;

    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 2);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_prepared_configured(
        &settings,
        &provider,
        super::scaffold_admission_tests::prepared_scaffold,
    )
    .await
    .expect("the shipped workspace prepares and selects its original before host readiness");
    eprintln!(
        "selected-coding-root-startup {}",
        serde_json::json!({
            "preparation_elapsed_ns": host.preparation_elapsed_ns(),
            "readiness_elapsed_ns": host.startup_readiness_elapsed_ns(),
        })
    );
    host.run_scenario(|host| {
        Box::pin(async move {
            let scenario_started = std::time::Instant::now();
            host.assert_fresh_prepared_workspace_original().await;
            host.input("Pass a workspace Task to the supplied child spec and return its typed candidate.")
                .await
                .unwrap();
            let root_id = host.context.actor.identity();
            let root_installation = host.context.observer.installation(root_id).await;
            let root_notebook = root_installation
                .tools
                .iter()
                .find(|tool| tool.name() == "haskell_sync")
                .expect("root notebook");
            assert!(root_notebook
                .effect_keys()
                .contains(&ActorEffectKey::Journal.into()));
            let mut pending = std::collections::VecDeque::new();
            let root = harness::model::AgentPath("/root".into());
            next_hosted_script_round(&mut requests, &mut pending, &root)
                .await
                .call(
                    "selected-workspace-input",
                    &tidepool_testing::fixture_source("bridge/facade/src/actor_host/selected_workspace_child_setup.hs"),
                );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_committed("selected-workspace-input");
            let (child_id, child_path) = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                loop {
                    if let Some(child) = host
                        .context
                        .forest
                        .inspect_host_graph()
                        .into_iter()
                        .find(|node| node.label == "worker")
                    {
                        assert!(child.terminal.is_none(), "{child:?}");
                        assert_eq!(child.creator, Some(root_id));
                        assert_eq!(child.supervisor_parent, Some(root_id));
                        assert_eq!(child.context_parent, None);
                        assert!(child.bound_worktree.is_some());
                        if let Some(binding) = host.context.binding(child.actor) {
                            if let Some(conversation) = binding.conversation() {
                                break (child.actor, conversation.identity().actor.clone());
                            }
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("selected child completes typed input admission and attaches its provider");
            let child_installation = host.context.observer.installation(child_id).await;
            assert_eq!(child_installation.context_parent, None);
            let child_notebook = child_installation
                .tools
                .iter()
                .find(|tool| tool.name() == "haskell_sync")
                .expect("supplied child spec notebook");
            let supplied_effects = [
                ActorEffectKey::Replies,
                ActorEffectKey::Commands,
                ActorEffectKey::Lookup,
                ActorEffectKey::BoundWorktree,
            ]
            .into_iter()
            .map(exomonad_tool::ToolEffectKey::from)
            .collect::<std::collections::HashSet<_>>();
            let actual_effects = child_notebook
                .effect_keys()
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(
                actual_effects,
                supplied_effects,
                "the child's notebook exposes the caller's supplied spec effect row"
            );
            assert!(!child_notebook
                .effect_keys()
                .contains(&ActorEffectKey::Journal.into()));

            next_hosted_script_round(&mut requests, &mut pending, &child_path)
                .await
                .call(
                    "selected-journal-refusal",
                    "import qualified Tidepool.Aeson.Value as Json\nrecord \"typed-source\" \"child\" Json.Null",
                );
            let child_after = next_hosted_script_round(&mut requests, &mut pending, &child_path).await;
            child_after.assert_failure("selected-journal-refusal", "Journal");
            let denied = child_after.settled_output("selected-journal-refusal");
            assert_eq!(denied["status"], "rejected", "{denied}");
            assert_eq!(denied["publication"]["status"], "notPublished", "{denied}");
            assert_eq!(denied["publication"]["reason"], "rejected", "{denied}");
            require_ghc_compile_rejection(&denied, &["Journal"])
                .unwrap_or_else(|error| panic!("{error}"));
            child_after.call(
                "selected-typed-reply",
                "import qualified Exomonad.Contrib.Types as Types\nrespond (Types.Produced (Types.Candidate (Types.taskSource sessionInput) [Types.obligation sessionInput] []))",
            );
            let child_replied = next_hosted_script_round(&mut requests, &mut pending, &child_path).await;
            let reply = child_replied.settled_output("selected-typed-reply");
            assert_eq!(reply["status"], "replied", "{reply}");
            assert_eq!(reply["publication"]["status"], "published", "{reply}");
            assert_eq!(
                reply["items"].as_array().unwrap().last().unwrap()["terminalTransfer"],
                "replyAccepted",
                "{reply}"
            );
            child_replied.finish();
            root_after.call(
                "selected-original-result",
                &tidepool_testing::fixture_source("bridge/facade/src/actor_host/selected_workspace_child_result.hs"),
            );
            let root_done = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_done.assert_value("selected-original-result", "True");
            root_done.finish();
            eprintln!(
                "selected-coding-scenario {}",
                serde_json::json!({"elapsed_ns": scenario_started.elapsed().as_nanos(), "completed": true})
            );
        })
    })
    .await;
}

/// A fresh-context child retains its typed input, allocates its own
/// declarations, and releases its distinct machine on retirement.
#[tokio::test]
async fn fresh_context_child_owns_and_retires_its_machine() {
    let campaign = TestCampaign::start_with_child_sessions().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let root_session = campaign
        .forest
        .actor_session(campaign.actor.identity())
        .expect("root actor has a session");

    let parent = dispatch_haskell_script(
        root.as_ref(),
        "data FreshParentInput = FreshParentInput Int deriving Show\ndata FreshParentReply = FreshParentReply Int deriving Show",
    ).await;
    assert_eq!(parent["status"], "committed", "{parent}");
    use tidepool_runtime::session::{observe_declaration_domain, RecoveryArtifactClosure, RecoveryDomainId, RecoveryNodeKind};
    let recovery_path = campaign.session_root.path().join("recovery.sqlite");
    let parent_domain = observe_declaration_domain(&recovery_path, RecoveryDomainId::Root)
        .unwrap().expect("the root declaration domain is retained");
    let parent_high_water = parent_domain.high_water;
    let root_for_setup = root.clone();
    let setup = tokio::spawn(async move {
        dispatch_haskell_script(
            root_for_setup.as_ref(),
            &tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/fresh_selected_child_setup.hs",
            ),
        )
        .await
    });
    let installation = campaign
        .next_deployment(
            "cross-session child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    let child_session = campaign
        .forest
        .actor_session(installation.actor.identity())
        .expect("child actor has a session");
    assert_ne!(
        child_session, root_session,
        "the explicit fresh-context launch must own its own machine"
    );

    campaign.acknowledge_native_spawn(&installation);
    let setup = setup.await.unwrap();
    assert_eq!(setup["status"], "committed", "{setup}");
    campaign
        .next_deployment(
            "fresh child typed activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == installation.actor.identity() =>
                {
                    Ok(activation)
                }
                LocalResidentDeployment::Retired { actor, terminal }
                    if actor == installation.actor.identity() =>
                {
                    panic!("fresh child retired before typed activation: {terminal:?}");
                }
                other => Err(other),
            },
        )
        .await;
    let child_before = observe_declaration_domain(&recovery_path, RecoveryDomainId::Session(child_session))
        .unwrap().expect("the typed child has retained its declaration domain").high_water;
    let store = display_output::open_run_store(campaign.session_root.path()).unwrap();
    let reply = campaign.drive_actor_output(&store, dispatch_haskell_script(
        installation.policy.as_ref(),
        "data FreshChildNominal = FreshChildNominal FreshParentInput\nfreshChildValue <- pure (FreshChildNominal sessionInput)\ndisplay (case freshChildValue of FreshChildNominal (FreshParentInput n) -> n + 1)",
    )).await;
    assert_eq!(committed_display_text(&reply), "42", "{reply}");
    let child_root = campaign
        .session_root
        .path()
        .join("haskell-session-children")
        .join(child_session.0.to_string());
    let child_domain = observe_declaration_domain(&recovery_path, RecoveryDomainId::Session(child_session))
        .unwrap().expect("the dedicated child declaration domain is retained");
    assert_eq!(child_domain.domain, RecoveryDomainId::Session(child_session));
    assert_eq!(child_domain.source_session, child_session.0);
    let authored = ((child_before.0 + 1)..=child_domain.high_water.0)
        .filter_map(|id| child_domain.declaration(tidepool_repr::Generation(id)).unwrap())
        .find(|node| node.kind == RecoveryNodeKind::Authored && node.exports.iter()
            .any(|export| export.identity.occurrence == "FreshChildNominal"))
        .expect("the child's cell retains its own authored nominal declaration");
    let generation = authored.generation;
    assert!(generation > parent_high_water, "{child_domain:?}");
    let identity = &authored.exports.iter()
        .find(|export| export.identity.occurrence == "FreshChildNominal").unwrap().identity;
    assert_eq!(identity.module, tidepool_repr::SessionModule::lib(generation).module_name());
    let native = authored.artifact_refs.iter().find_map(|id| match child_domain.artifact(*id).unwrap() {
        Some(RecoveryArtifactClosure::Home(reference)) if reference.unit == identity.unit && reference.module == identity.module => Some(reference),
        _ => None,
    }).expect("the authored child retains its exact original native product");
    tidepool_toolchain::recovery_artifacts::verify_materialized_ref(&child_root, &native).unwrap();
    use tidepool_toolchain::artifact_inventory::ArtifactDescriptor;
    let native_id = ArtifactDescriptor::from_recovery_product(&native).id;
    let context = child_domain.declaration_context(generation).unwrap();
    let selected_interfaces = context.snapshot().compiler_roles().into_iter()
        .filter_map(|role| match child_domain.artifact(role.interface()).unwrap() {
            Some(RecoveryArtifactClosure::Join(reference)) => Some(reference),
            _ => None,
        }).collect::<Vec<_>>();
    for root in &authored.lexical_roots {
        let projection = selected_interfaces.iter().find(|projection|
            root.unit == projection.unit && root.module == projection.module)
            .expect("the child's newly issued lexical projection is selected in its exact context");
        tidepool_toolchain::recovery_artifacts::verify_materialized_join(&child_root, projection).unwrap();
    }
    assert!(!authored.lexical_roots.is_empty());
    let dependencies = context.snapshot().binding_dependencies().unwrap();
    let mut reached = std::collections::BTreeSet::new();
    let mut pending = dependencies.iter().flat_map(|(source,target,_)| [*source,*target])
        .filter(|node| node.artifact() == native_id).collect::<std::collections::VecDeque<_>>();
    while let Some(source) = pending.pop_front() {
        if reached.insert(source) {
            pending.extend(dependencies.iter().filter_map(|(owner,dependency,_)|
                (*owner == source).then_some(*dependency)));
        }
    }
    let selected_ids = reached.iter().map(|node| node.artifact()).collect::<std::collections::BTreeSet<_>>();
    let inherited_projections = selected_ids.into_iter().filter_map(|id| {
        match child_domain.artifact(id).unwrap() {
            Some(RecoveryArtifactClosure::Join(reference))
                if parent_domain.artifact(id).unwrap() == Some(RecoveryArtifactClosure::Join(reference.clone())) => Some(reference),
            _ => None,
        }
    }).collect::<Vec<_>>();
    assert!(!inherited_projections.is_empty(),
        "the exact child original must depend on an interface retained before child creation");
    for projection in &inherited_projections {
        tidepool_toolchain::recovery_artifacts::verify_materialized_join(&child_root, projection).unwrap();
    }
    let parent_after = observe_declaration_domain(&recovery_path, RecoveryDomainId::Root)
        .unwrap().expect("the parent declaration domain remains retained");
    assert!(parent_after.declaration(generation).unwrap().is_none(),
        "the child's issued declaration must be absent from the parent's domain");

    let replied = dispatch_haskell_script(
        installation.policy.as_ref(),
        "respond (case sessionInput of FreshParentInput n -> FreshParentReply (n + 1))",
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");

    installation
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "test teardown".into(),
            diagnostic: None,
        })
        .await
        .expect("child shuts down");
    campaign
        .next_deployment(
            "cross-session child retirement",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::Retired { actor, terminal }
                    if actor == installation.actor.identity() =>
                {
                    Ok(terminal)
                }
                other => Err(other),
            },
        )
        .await;
    assert_eq!(
        campaign.forest.session_state_of(child_session),
        tidepool_runtime::session::ResidentSessionState::Gone,
        "the dedicated child session must be released once its actor retires"
    );
    let result = campaign
        .drive_actor_output(
            &store,
            dispatch_haskell_script(
                root.as_ref(),
                "Right completed <- await (result freshSelectedChild)\ndisplay (case completed of FreshParentReply n -> n)",
            ),
        )
        .await;
    assert_eq!(committed_display_text(&result), "42", "{result}");
})).await;
}
