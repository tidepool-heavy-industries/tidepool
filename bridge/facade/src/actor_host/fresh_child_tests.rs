use super::display_output;
use super::test_campaign::{committed_display_text, dispatch_haskell_script, TestCampaign};
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
        let setup = dispatch_haskell_script(root.as_ref(),
            &tidepool_testing::fixture_source("bridge/facade/src/actor_host/source_setup.hs")).await;
        assert_eq!(setup["status"], "committed", "{setup}");
        let producer = campaign.next_deployment("source producer admission", Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            }).await;
        assert_eq!(campaign.forest.actor_session(producer.actor.identity()), Some(root_session),
            "captured-context producer retains its issuing machine");
        let first = dispatch_haskell_script(producer.policy.as_ref(),
            "reportProgress (ProgressNote 1 (+ sessionInput))").await;
        assert_eq!(first["status"], "committed", "{first}");
        let installed = dispatch_haskell_script(root.as_ref(),
            &tidepool_testing::fixture_source("bridge/facade/src/actor_host/fresh_result_source_actor.hs")).await;
        assert_eq!(installed["status"], "committed", "{installed}");
        let collector = campaign.forest.inspect_host_graph().into_iter()
            .find(|node| node.label == "ordered-source-collector").unwrap();
        let collector_session = campaign.forest.actor_session(collector.actor).unwrap();
        assert_ne!(collector_session, root_session, "source observation crosses actual machines");
        let published = dispatch_haskell_script(producer.policy.as_ref(),
            "reportProgress (ProgressNote 2 (* sessionInput))\nreportProgress (ProgressNote 3 (subtract sessionInput))\nrespond (42 :: Int)").await;
        assert_eq!(published["status"], "replied", "{published}");
        let settled = dispatch_haskell_script(root.as_ref(),
            "settled <- watch (Just \"fresh-source-result\") (result answer)").await;
        assert_eq!(settled["status"], "committed", "{settled}");
        campaign.next_deployment("source settlement is ready", Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::WatchChanged { notification } if notification.owner == root_id && notification.label == "fresh-source-result" => {
                    assert_eq!(notification.transition, exomonad_actor::WatchTransition::Ready);
                    Ok(())
                }
                other => Err(other),
            }).await;
        let drained = dispatch_haskell_script(root.as_ref(), "drainActor collector").await;
        assert_eq!(drained["status"], "committed", "{drained}");
        campaign.next_deployment("source collector retirement", Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::Retired { actor, terminal } if actor == collector.actor => Ok(terminal),
                other => Err(other),
            }).await;
        assert_eq!(campaign.forest.session_state_of(collector_session),
            tidepool_runtime::session::ResidentSessionState::Gone);
        let store = display_output::open_run_store(campaign.session_root.path()).unwrap();
        let collected = campaign.drive_actor_output(&store, dispatch_haskell_script(root.as_ref(),
            "collected <- awaitExit collector\ndisplay (case collected of { Completed values -> reverse values == [13, 30, -7, -1, 42]; _ -> False })")).await;
        assert_eq!(committed_display_text(&collected), "True", "{collected}");
    })).await;
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
                let setup_policy = root.clone();
                let setup = tokio::spawn(async move {
                    dispatch_haskell_script(
                        setup_policy.as_ref(),
                        &tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/fresh_callable_result_setup.hs",
                        ),
                    )
                    .await
                });
                let child = campaign
                    .next_deployment(
                        "callable child policy installation",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                            other => Err(other),
                        },
                    )
                    .await;
                let child_id = child.actor.identity();
                let child_session = campaign.forest.actor_session(child_id).unwrap();
                assert_ne!(
                    child_session, root_session,
                    "callable response crosses actual machines"
                );
                campaign.acknowledge_native_spawn(&child);
                let setup = setup.await.unwrap();
                assert_eq!(setup["status"], "committed", "{setup}");
                let root_id = campaign.actor.identity();
                campaign
                    .next_deployment(
                        "callable child typed activation",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::SessionReady { activation }
                                if activation.id.actor() == child_id =>
                            {
                                Ok(activation)
                            }
                            other => Err(other),
                        },
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
    let parent_manifest_path = campaign.session_root.path().join("root-declarations.json");
    let parent_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&parent_manifest_path).unwrap()).unwrap();
    let parent_high_water = parent_manifest["high_water"].as_u64().unwrap();
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
    let child_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(child_root.join("declarations.json")).unwrap())
            .unwrap();
    assert_eq!(child_manifest["source_session"], child_session.0);
    let authored = child_manifest["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| {
            node["kind"] == "authored"
                && node["exports"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|export| export["identity"]["occurrence"] == "FreshChildNominal")
        })
        .unwrap();
    let generation = authored["id"].as_u64().unwrap();
    assert!(generation > parent_high_water, "{child_manifest}");
    let identity = authored["exports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|export| export["identity"]["occurrence"] == "FreshChildNominal")
        .unwrap();
    assert_eq!(
        identity["identity"]["module"],
        tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation)).module_name()
    );
    let projections: Vec<tidepool_toolchain::recovery_artifacts::RecoveryJoinRef> = child_manifest
        ["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|artifact| artifact["kind"] == "join")
        .map(|artifact| serde_json::from_value(artifact["reference"].clone()).unwrap())
        .collect();
    assert!(
        authored["lexical_roots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|root| {
                projections.iter().any(|projection| {
                    root["unit"] == projection.unit && root["module"] == projection.module
                })
            }),
        "the public lexical root must be an issued interface: {child_manifest}"
    );
    let native: tidepool_toolchain::recovery_artifacts::RecoveryArtifactRef =
        serde_json::from_value(
            child_manifest["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|artifact| {
                    artifact["kind"] == "home"
                        && artifact["reference"]["unit"] == identity["identity"]["unit"]
                        && artifact["reference"]["module"] == identity["identity"]["module"]
                })
                .unwrap()["reference"]
                .clone(),
        )
        .unwrap();
    tidepool_toolchain::recovery_artifacts::verify_materialized_ref(&child_root, &native).unwrap();
    use tidepool_toolchain::artifact_inventory::{ArtifactDescriptor, ArtifactId};
    let mut reached = std::collections::BTreeSet::new();
    let mut pending =
        std::collections::VecDeque::from([ArtifactDescriptor::from_recovery_product(&native).id]);
    let dependencies: Vec<(ArtifactId, ArtifactId)> = child_manifest["artifact_dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| {
            (
                serde_json::from_value(edge["source"].clone()).unwrap(),
                serde_json::from_value(edge["target"].clone()).unwrap(),
            )
        })
        .collect();
    while let Some(source) = pending.pop_front() {
        if reached.insert(source) {
            pending.extend(
                dependencies
                    .iter()
                    .filter_map(|(owner, dependency)| (*owner == source).then_some(*dependency)),
            );
        }
    }
    assert!(projections.iter().any(|projection| reached.contains(&ArtifactDescriptor::from_recovery_join(projection).id)),
        "original native declaration must retain its selected interface dependency: {child_manifest}");
    for projection in &projections {
        tidepool_toolchain::recovery_artifacts::verify_materialized_join(&child_root, projection)
            .unwrap();
    }
    let parent_after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&parent_manifest_path).unwrap()).unwrap();
    assert!(!parent_after["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["exports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|export| export["identity"]["occurrence"] == "FreshChildNominal")));

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
