use super::*;

#[test]
fn root_never_bound_is_true_exactly_when_the_binding_file_is_absent() {
    // Regression: a root coordination failure before this run's root ever
    // reached a queue-ready binding used to leave the host running,
    // holding the host incarnation lease and every worktree binding lock a
    // fresh run of the same workspace needs, sometimes for minutes.
    let root = tempfile::tempdir().unwrap();
    let binding_path = root.path().join("root-binding.json");
    assert!(root_never_bound(&binding_path));

    std::fs::write(
        &binding_path,
        "not even a real binding, just proof of writing",
    )
    .unwrap();
    assert!(!root_never_bound(&binding_path));
}

#[test]
fn operator_role_has_journal_without_widening_child_roles() {
    let operator = operator_effective_role(exomonad_actor::ResearchPolicy::default());
    assert!(operator
        .effect_keys()
        .contains(&exomonad_actor::ActorEffectKey::Journal));
    assert!(operator.haskell_effects_type().contains("Journal"));

    for child in [
        exomonad_actor::EffectiveRole::research(),
        exomonad_actor::EffectiveRole::coding(),
        exomonad_actor::EffectiveRole::integration(),
    ] {
        assert!(!child
            .effect_keys()
            .contains(&exomonad_actor::ActorEffectKey::Journal));
    }
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
        "requestWithProbe",
        "progressProbe",
        "retainedProgressProbe",
        "childProbe",
        "childProgressProbe",
    ];
    let artifacts = tidepool_runtime::compile_targets(
        include_str!("typed_site_return_contract.hs"),
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
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(root.as_ref(), include_str!("roster_setup.hs")).await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let mut children = Vec::new();
    while children.len() < 2 {
        let child =
            campaign
                .next_deployment("roster child admission", Duration::from_secs(30), |event| {
                    match event {
                        LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                        LocalResidentDeployment::Retired { actor, terminal } => {
                            panic!("{actor:?}: {terminal:?}")
                        }
                        other => Err(other),
                    }
                })
                .await;
        children.push(child);
    }
    for policy in
        std::iter::once(root.as_ref()).chain(children.iter().map(|child| child.policy.as_ref()))
    {
        let observed = dispatch_haskell_script(policy, include_str!("roster_observe.hs")).await;
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
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// The composition root's `ChildSessionFactory` (installed on `forest` in
/// `actor_host.rs::compile_root`/`run`, per-actor-machines parcel 2) builds a
/// session that can actually run a cell — not just an idle placeholder. This
/// runs the SAME compiled driver turn the root itself bootstraps with
/// (`campaign.program`), directly against a factory-built machine, mirroring
/// `compile_root`'s own bootstrap sequence exactly. Nothing here goes through
/// `capture_decoded`/`child_session_eligibility` (that wiring is a later
/// parcel); this only proves the factory itself, called directly, produces a
/// working machine.
#[tokio::test]
async fn composition_root_child_session_factory_runs_a_cell() {
    let campaign = test_campaign::TestCampaign::start().await;
    let child_session_id = tidepool_runtime::session::fresh_session_id();
    let mut child_machine = (campaign.child_session_factory)(child_session_id)
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

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

// `descendants_observe.hs` composes existing primitives — `actorContext`
// (self identity), `snapshot`/`listAgentsFull` (the same registry
// `observeAgent` reads), and `creationTree` (a pure roster filter by
// `rosterCreatorId`/`rosterCreatorIncarnation`) — into "my live
// descendants". No new effect is needed: the registry already scopes
// `listAgentsFull` to what the caller may observe, and `creationTree` is
// already exported for exactly this composition.
#[tokio::test]
async fn descendants_list_the_spawn_tree_and_drop_a_retired_leaf() {
    // Two fork levels: the research policy's depth budget of 1 lets the
    // child itself unfold one further generation (the grandchild), which
    // then has none left.
    let mut campaign =
        test_campaign::TestCampaign::start_with_research_policy(exomonad_actor::ResearchPolicy {
            maximum_depth: 1,
            maximum_active_children: Some(1),
            default_depth: 1,
        })
        .await;
    let root = campaign.root_installation.policy.clone();
    let root_id = campaign.actor.identity();

    let root_for_setup = root.clone();
    let setup = tokio::spawn(async move {
        dispatch_haskell_script(
            root_for_setup.as_ref(),
            include_str!("descendants_setup.hs"),
        )
        .await
    });
    let child_installation = campaign
        .next_deployment(
            "descendants child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    let child_id = child_installation.actor.identity();
    campaign.authority.install_grant(
        child_id.into(),
        worktree_grant(child_installation.effective_role.role()),
    );
    child_installation
        .fork_gate
        .as_ref()
        .expect("child fork gate")
        .mark_ready()
        .unwrap();
    let setup = setup.await.unwrap();
    assert_eq!(setup["status"], "committed", "{setup:?}");

    let child_policy = child_installation.policy.clone();
    let child_spawn = tokio::spawn(async move {
        dispatch_haskell_script(
            child_policy.as_ref(),
            include_str!("descendants_child_spawn.hs"),
        )
        .await
    });
    let grandchild_installation = campaign
        .next_deployment(
            "descendants grandchild policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        grandchild_installation.actor.identity().into(),
        worktree_grant(grandchild_installation.effective_role.role()),
    );
    grandchild_installation
        .fork_gate
        .as_ref()
        .expect("grandchild fork gate")
        .mark_ready()
        .unwrap();
    let child_spawn = child_spawn.await.unwrap();
    assert_eq!(child_spawn["status"], "committed", "{child_spawn:?}");

    let observed =
        dispatch_haskell_script(root.as_ref(), include_str!("descendants_observe.hs")).await;
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

    let after_retirement =
        dispatch_haskell_script(root.as_ref(), include_str!("descendants_observe.hs")).await;
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

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn root_recovery_replays_lost_workbench_reply_without_repeating_effects() {
    use exomonad_actor::ResidentToolEndpoint as _;

    let mut campaign = test_campaign::TestCampaign::start().await;
    let target = campaign
        .forest
        .new_workbench(
            "notification-target".into(),
            exomonad_actor::EffectiveRole::root(),
        )
        .await
        .unwrap();
    let target_id = target.identity();
    let source = format!(
        "import qualified Tidepool.Effects.Core as RecoveryEffects\nsend (RecoveryEffects.NotifyWith ({}, {}) \"counted-recovery-effect\") >> pure ()",
        target_id.id.0, target_id.incarnation.0
    );
    let request = ToolInvocation {
        context: Some(ToolInvocationContext {
            context_call_id: Some("lost-recovery-call".into()),
            thread_id: "retained-native-thread".into(),
            turn_id: "native-turn".into(),
            call_id: "native-call".into(),
            namespace: None,
        }),
        name: exomonad_actor::HASKELL_TOOL.into(),
        arguments: exomonad_tool::ToolArguments::Raw(source),
    };
    let policy = campaign.root_installation.policy.clone();
    let mut first = tokio::spawn(policy.dispatch_boxed(request.clone()));
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
        policy.dispatch_boxed(request.clone()),
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
        })
        .await
        .unwrap();
    (&mut campaign.hosted).await.unwrap();
    let (successor, task) = campaign
        .forest
        .recover_program_root(
            campaign.actor.identity(),
            "recovered-root".into(),
            exomonad_actor::EffectiveRole::root(),
            campaign.program.clone(),
        )
        .await
        .unwrap();
    assert_ne!(successor.identity(), campaign.actor.identity());
    assert!(campaign
        .forest
        .recover_program_root(
            campaign.actor.identity(),
            "duplicate-recovery".into(),
            exomonad_actor::EffectiveRole::root(),
            campaign.program.clone(),
        )
        .await
        .is_err());
    let successor_policy = exomonad_actor::ResidentInteractivePolicy::local(successor.clone());
    let mut retry = tokio::spawn(successor_policy.dispatch_boxed(request.clone()));
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
    let conflict = successor_policy.dispatch_boxed(altered).await.unwrap_err();
    assert!(conflict.to_string().contains("different Haskell input"));
    campaign.forest.shutdown().await;
    task.await.unwrap();
}

#[tokio::test]
async fn forest_operator_survives_model_root_recovery() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let operator = campaign
        .forest
        .new_workbench("operator".into(), exomonad_actor::EffectiveRole::root())
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
                request: tidepool_runtime::session::WorkbenchRequest::from_cell_input(source),
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
    let requested = submit(&operator, include_str!("operator_request.hs")).await;
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
    let replied =
        dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput + 1 :: Int)").await;
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
        })
        .await
        .unwrap();
    campaign.hosted.await.unwrap();
    assert_eq!(
        campaign.forest.resident_session_state(),
        ResidentSessionState::Reusable,
        "actor failure must not imply that the resident machine is safe to replace"
    );
    let (replacement, task) = campaign
        .forest
        .recover_program_root(
            campaign.actor.identity(),
            "replacement".into(),
            exomonad_actor::EffectiveRole::root(),
            campaign.program.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        campaign.forest.resident_session_state(),
        ResidentSessionState::Reusable
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
    campaign.forest.shutdown().await;
    task.await.unwrap();
    assert!(operator.terminal().get().is_some());
}

#[tokio::test]
async fn actor_sources_capture_current_then_deliver_every_publication_and_settlement() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(root.as_ref(), include_str!("source_setup.hs")).await;
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
    let installed = dispatch_haskell_script(root.as_ref(), include_str!("source_actor.hs")).await;
    assert_eq!(installed["status"], "committed", "{installed:?}");
    for item in installed["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{installed:?}");
    }
    let rejected = dispatch_haskell_script_result(
        root.as_ref(),
        include_str!("source_replacement_rejected.hs"),
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
    let settled = dispatch_haskell_script(root.as_ref(), "settled <- watch (case watchLabel \"source-settled\" of { Right label -> label; Left _ -> error \"fixture label\" }) (awaitResponse answer)").await;
    assert_eq!(settled["status"], "committed", "{settled:?}");
    campaign.await_watch_ready().await;
    let collected = dispatch_haskell_script(
        root.as_ref(),
        "drainActor collector2\nresult <- awaitExit collector2\ncase result of { Completed values -> reverse values == [13, 30, -7, -1, 42]; _ -> False }",
    )
    .await;
    assert_eq!(collected["status"], "committed", "{collected:?}");
    assert!(collected.to_string().contains("True"), "{collected:?}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn progress_retains_closures_and_watch_snapshots_across_calls() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(root.as_ref(), include_str!("progress_setup.hs")).await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let child = campaign
        .next_deployment(
            "progress child policy installation",
            Duration::from_secs(30),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
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
    let captured =
        dispatch_haskell_script(root.as_ref(), include_str!("progress_observe.hs")).await;
    assert_eq!(captured["status"], "committed", "{captured:?}");
    assert!(captured.to_string().contains("(13, 30)"), "{captured:?}");
    assert_eq!(captured["items"][7]["output"], "40", "{captured:?}");
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond (42 :: Int)").await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    let stopped = dispatch_haskell_script(root.as_ref(), "stopAgent worker").await;
    assert_eq!(stopped["status"], "committed", "{stopped:?}");
    let retained =
        dispatch_haskell_script(root.as_ref(), include_str!("progress_retained.hs")).await;
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
        })
        .await
        .unwrap();
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn request_update_keeps_original_request_and_fences_terminal_delivery() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup =
        dispatch_haskell_script(root.as_ref(), include_str!("request_update_setup.hs")).await;
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
        })
        .await
        .unwrap();
    campaign.hosted.await.unwrap();
}

#[test]
fn provider_failure_notice_preserves_identity_and_old_inbox_payloads() {
    let notice = super::DurableActorEvent::Typed(super::TypedActorEvent::ProviderTurnFailed {
        revision: 10,
        actor: exomonad_actor::ActorRef {
            id: exomonad_actor::ActorId(7),
            incarnation: exomonad_actor::Incarnation(3),
        },
        thread: "provider-thread".into(),
        turn: "provider-turn".into(),
        failure: exomonad_agent::ProviderFailure::Other("unknown provider code".into()),
    });
    let encoded = serde_json::to_string(&notice).unwrap();
    assert_eq!(
        serde_json::from_str::<super::DurableActorEvent>(&encoded).unwrap(),
        notice
    );
    assert!(notice.render(None).contains("7@3"));
    let legacy = serde_json::from_str::<super::DurableActorEvent>("\"old event\"").unwrap();
    assert_eq!(legacy.render(None), "old event");
}

#[tokio::test]
async fn provider_failure_publication_deduplicates_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let rows = directory.path().join("rows");
    let cursor = directory.path().join("cursor");
    let inbox = Arc::new(ActorInbox::open(rows.clone(), cursor.clone()).unwrap());
    let notice = |turn: &str, revision| {
        DurableActorEvent::Typed(TypedActorEvent::ProviderTurnFailed {
            actor: exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            thread: "thread".into(),
            turn: turn.into(),
            revision,
            failure: exomonad_agent::ProviderFailure::RequestRejected,
        })
    };
    publish_inbox_event(inbox.clone(), notice("first", 10))
        .await
        .unwrap();
    publish_inbox_event(inbox.clone(), notice("first", 10))
        .await
        .unwrap();
    assert_eq!(inbox.pending().unwrap().len(), 1);
    inbox.acknowledge(1).unwrap();
    drop(inbox);
    let inbox = Arc::new(ActorInbox::open(rows, cursor).unwrap());
    publish_inbox_event(inbox.clone(), notice("first", 10))
        .await
        .unwrap();
    assert!(inbox.pending().unwrap().is_empty());
    publish_inbox_event(inbox.clone(), notice("second", 20))
        .await
        .unwrap();
    assert_eq!(inbox.pending().unwrap().len(), 1);
}

#[test]
fn fork_effort_defaults_low_and_preserves_explicit_overrides() {
    use exomonad_actor::ForkEffort;
    use exomonad_agent::{BackendThreadId, InteractiveLaunchMode, ReasoningEffort};
    let fork = InteractiveLaunchMode::Fork {
        parent: BackendThreadId("parent".into()),
        after_call: "call".into(),
    };
    for default in [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ] {
        assert_eq!(
            super::launch_effort(&fork, default, None),
            ReasoningEffort::Low
        );
        for (requested, selected) in [
            (ForkEffort::Low, ReasoningEffort::Low),
            (ForkEffort::Medium, ReasoningEffort::Medium),
            (ForkEffort::High, ReasoningEffort::High),
        ] {
            assert_eq!(
                super::launch_effort(&fork, default, Some(requested)),
                selected
            );
        }
        assert_eq!(
            super::launch_effort(&InteractiveLaunchMode::Fresh, default, None),
            default
        );
        assert_eq!(
            super::launch_effort(
                &InteractiveLaunchMode::Resume(BackendThreadId("retained".into())),
                default,
                None
            ),
            default
        );
    }
}

use exomonad_agent::{
    AgentBackendError, InteractiveAgentCommand, InteractiveAgentSpec, InteractiveFuture,
};
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
use exomonad_worktree::WorktreeSpec;

fn durable_root(actor: ActorRef) -> exomonad_actor::DurableActorRecord {
    exomonad_actor::DurableActorRecord {
        admission: exomonad_actor::DurableActorAdmission {
            actor,
            label: "exomonad-root".into(),
            creator: None,
            supervisor_parent: None,
            context_parent: None,
            actor_path: None,
            role: "root".into(),
            descendant_depth: 8,
            descendant_active_children: None,
            model: None,
            effort: None,
            instructions: None,
            launch_worktrees: Vec::new(),
            source_layer: Vec::new(),
        },
        application: Some(exomonad_actor::DurableActorApplication {
            binding_path: std::path::PathBuf::from(format!(
                "binding-{}-{}.json",
                actor.id.0, actor.incarnation.0
            )),
            conversation: Some(format!("conversation-{}", actor.id.0)),
            accepted_source: Some("source-revision".into()),
        }),
        terminal: None,
    }
}

fn durable_child(
    actor: ActorRef,
    conversation: Option<&str>,
) -> exomonad_actor::DurableActorRecord {
    let mut record = durable_root(actor);
    record.admission.role = "coding".into();
    record.admission.creator = Some(ActorRef::first(exomonad_actor::ActorId(1)));
    record.application = conversation.map(|conversation| exomonad_actor::DurableActorApplication {
        binding_path: std::path::PathBuf::from(format!(
            "binding-{}-{}.json",
            actor.id.0, actor.incarnation.0
        )),
        conversation: Some(conversation.into()),
        accepted_source: Some("source-revision".into()),
    });
    record
}

#[test]
fn actor_recovery_records_the_published_source_revision() {
    let project = tempfile::tempdir().unwrap();
    let run = tempfile::tempdir().unwrap();
    let authored = project.path().join(".exomonad/Project");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        project.path().join(".exomonad/config.toml"),
        "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Work']\n",
    )
    .unwrap();
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
    latest.terminal = Some(exomonad_actor::DurableActorTerminal {
        kind: exomonad_actor::ActorExitKind::Completed,
        summary: "retired".into(),
    });

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
fn repeated_recovery_uses_only_the_latest_logical_actor_incarnation() {
    let root = ActorRef {
        id: exomonad_actor::ActorId(1),
        incarnation: exomonad_actor::Incarnation(4),
    };
    let child = exomonad_actor::ActorId(2);
    let first = durable_child(
        ActorRef {
            id: child,
            incarnation: exomonad_actor::Incarnation(1),
        },
        Some("old-conversation"),
    );
    let second = durable_child(
        ActorRef {
            id: child,
            // Actor incarnations can advance independently of the host
            // generation and may happen to have the same number.
            incarnation: exomonad_actor::Incarnation(4),
        },
        Some("latest-conversation"),
    );
    let selected = latest_recoverable_actor_records(&[first.clone(), second.clone()], root);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].admission.actor, second.admission.actor);

    // A crash after the successor admission must fence the older
    // conversation instead of launching it yet again.
    let unpublished = durable_child(
        ActorRef {
            id: child,
            incarnation: exomonad_actor::Incarnation(5),
        },
        None,
    );
    assert!(
        latest_recoverable_actor_records(&[first.clone(), second.clone(), unpublished], root,)
            .is_empty()
    );

    let mut retired = second;
    retired.terminal = Some(exomonad_actor::DurableActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "done".into(),
    });
    assert!(latest_recoverable_actor_records(&[first, retired], root).is_empty());
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
    assert_eq!(report.unavailable, vec!["2-1"]);
}

#[test]
fn recovery_fails_closed_when_root_process_evidence_is_missing() {
    let run = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(run.path().join("1-1")).unwrap();
    let report = stop_predecessor_processes(run.path()).unwrap();
    assert!(!report.root_available());
    assert_eq!(report.unavailable, vec!["1-1"]);
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
    assert_eq!(report.unavailable, vec!["2-1"]);
    assert!(!socket_root.exists());
}

fn test_delivery_dependencies(
    root: &Path,
    actor: ActorRef,
) -> (
    InputProducerId,
    Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
) {
    (
        input_producer_id(root, actor, "test-inbox").unwrap(),
        Mutex::new(BTreeMap::new()),
    )
}

fn normalized_prompt(prompt: &str) -> String {
    prompt.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn activation_delivery_refuses_duplicates_and_stale_sequences() {
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let other_actor = ActorRef::first(exomonad_actor::ActorId(8));
    assert!(accepts_activation_id(actor, 0, actor, 1));
    assert!(!accepts_activation_id(actor, 1, actor, 1));
    assert!(accepts_activation_id(actor, 1, actor, 3));
    assert!(!accepts_activation_id(actor, 3, actor, 2));
    assert!(!accepts_activation_id(actor, 3, other_actor, 4));
}

#[test]
fn native_input_producer_binds_run_inbox_and_actor_incarnation() {
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let first = input_producer_id(Path::new("/runs/first"), actor, "actor-inbox").unwrap();
    let reconnect = input_producer_id(Path::new("/runs/first"), actor, "actor-inbox").unwrap();
    let another_run = input_producer_id(Path::new("/runs/second"), actor, "actor-inbox").unwrap();
    let another_incarnation = input_producer_id(
        Path::new("/runs/first"),
        ActorRef {
            id: actor.id,
            incarnation: exomonad_actor::Incarnation(2),
        },
        "actor-inbox",
    )
    .unwrap();

    assert_eq!(first, reconnect);
    assert_ne!(first, another_run);
    assert_ne!(first, another_incarnation);
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

pub(super) async fn dispatch_haskell_script(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    script: &str,
) -> serde_json::Value {
    dispatch_haskell_script_result(endpoint, script)
        .await
        .unwrap_or_else(|error| panic!("Haskell script failed:\n{script}\n\n{error}"))
}

pub(super) async fn dispatch_haskell_script_result(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    script: &str,
) -> Result<serde_json::Value, exomonad_actor::ResidentToolError> {
    let call_id = uuid::Uuid::new_v4().simple().to_string();
    let result = endpoint
        .dispatch_boxed(ToolInvocation {
            context: Some(ToolInvocationContext {
                context_call_id: Some(call_id.clone()),
                thread_id: "actor-host-vertical".into(),
                turn_id: call_id.clone(),
                call_id: call_id.clone(),
                namespace: Some("haskell".into()),
            }),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(script.into()),
        })
        .await;
    endpoint
        .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary {
            thread_id: "actor-host-vertical".into(),
            call_id,
        })
        .await
        .expect("recorded tool completion");
    result
}

pub(super) async fn dispatch_structured_tool(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let call_id = uuid::Uuid::new_v4().simple().to_string();
    let result = endpoint
        .dispatch_boxed(ToolInvocation {
            context: Some(ToolInvocationContext {
                context_call_id: Some(call_id.clone()),
                thread_id: "actor-host-vertical".into(),
                turn_id: call_id.clone(),
                call_id: call_id.clone(),
                namespace: Some(name.into()),
            }),
            name: name.into(),
            arguments: ToolArguments::Structured(arguments),
        })
        .await
        .unwrap_or_else(|error| panic!("{name} tool failed: {error}"));
    endpoint
        .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary {
            thread_id: "actor-host-vertical".into(),
            call_id,
        })
        .await
        .expect("recorded tool completion");
    result
}

pub(super) async fn dispatch_lookup(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    queries: &[&str],
) -> serde_json::Value {
    dispatch_structured_tool(
        endpoint,
        "lookup",
        serde_json::json!({ "queries": queries }),
    )
    .await
}

pub(super) async fn dispatch_status(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    view: &str,
) -> serde_json::Value {
    dispatch_structured_tool(endpoint, "status", serde_json::json!({ "view": view })).await
}

#[tokio::test]
async fn idle_application_waits_for_current_host_attachment_despite_retained_binding() {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let session = TmuxSession::with_socket(
        format!("exomonad_binding_{}", &suffix[..8]),
        format!("exomonad-binding-{}", &suffix[..8]),
    )
    .unwrap();
    let pane = session
        .create(&TmuxLaunch {
            window_name: "Root".into(),
            cwd: std::env::temp_dir(),
            program: "sleep".into(),
            args: vec!["60".into()],
            environment: std::collections::BTreeMap::new(),
            unset_environment: BTreeSet::new(),
        })
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("binding.json");
    let thread = BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into());
    exomonad_agent::accept_interactive_session_binding(
        &path,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        thread.clone(),
        None,
    )
    .await
    .unwrap();
    let host = crate::host_dynamic_tools::HostDynamicToolService::new(
        crate::host_dynamic_tools::test_endpoint(),
        path.clone(),
        Some(thread.clone()),
    )
    .unwrap();
    let control = host.control();
    let socket = root.path().join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(host.serve(listener));
    let actor = ActorRef::first(exomonad_actor::ActorId(1));
    let binding = discover_interactive_binding(
        actor,
        InteractiveBindingRequest {
            control,
            path: path.clone(),
            expected: None,
        },
        &session,
        &pane,
    );
    tokio::pin!(binding);

    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut binding)
            .await
            .is_err()
    );
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .no_proxy()
        .build()
        .unwrap();
    let response = client
        .post("http://localhost/v1/dynamic-tools/session")
        .json(&serde_json::json!({
            "protocolVersion": exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
            "threadId": thread.0
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), &mut binding)
            .await
            .unwrap()
            .unwrap()
            .id(),
        &thread
    );
    session.kill().await.unwrap();
    server.abort();
}

#[test]
fn root_instructions_preserve_idle_and_resume_contracts() {
    let role = exomonad_actor::EffectiveRole::root();
    let fresh = developer_instructions(&role, &InteractiveLaunchMode::Fresh);
    let resumed = developer_instructions(
        &role,
        &InteractiveLaunchMode::Resume(BackendThreadId("retained-thread".into())),
    );
    assert!(fresh.starts_with(PromptId::ExomonadRoot.body()));
    assert!(!fresh.contains(PromptId::ExomonadBase.body()));
    assert!(!resumed.contains(PromptId::ExomonadBase.body()));
    assert!(!fresh.contains(PromptId::RecreatedRoot.body()));
    assert!(resumed.starts_with(PromptId::ExomonadRoot.body()));
    assert_eq!(resumed.matches(PromptId::RecreatedRoot.body()).count(), 1);
    assert!(normalized_prompt(&resumed).contains("Previous actor handles"));
    let root_effects = role.haskell_effects_type();
    for projection in [
        role.prompt_profile(),
        root_effects.as_str(),
        "native_tools=Coding",
        "workspace=WritableBound",
    ] {
        assert!(fresh.contains(projection), "missing {projection}: {fresh}");
    }
}

#[tokio::test]
async fn abnormal_root_reuses_only_a_queue_ready_conversation() {
    let root = tempfile::tempdir().unwrap();
    let binding = root.path().join("root-binding.json");
    let thread = BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into());
    exomonad_agent::accept_interactive_session_binding(
        &binding,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        thread.clone(),
        None,
    )
    .await
    .unwrap();

    let completed = ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "done".into(),
    };
    assert!(
        root_recovery_launch_mode(&binding, &completed, ResidentSessionState::Unavailable,)
            .await
            .unwrap()
            .is_none()
    );
    assert!(root_recovery_launch_mode(
        &binding,
        &ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "operator cancelled".into(),
        },
        ResidentSessionState::Gone,
    )
    .await
    .unwrap()
    .is_none());

    let failed = ActorTerminal {
        kind: ActorExitKind::Failed,
        summary: "stale reply target".into(),
    };
    let (mode, retained) =
        root_recovery_launch_mode(&binding, &failed, ResidentSessionState::Reusable)
            .await
            .unwrap()
            .expect("failed roots are recreated");
    assert_eq!(mode, InteractiveLaunchMode::Resume(thread.clone()));
    assert_eq!(retained.id(), &thread);

    let missing = root.path().join("missing-binding.json");
    assert!(
        root_recovery_launch_mode(&missing, &failed, ResidentSessionState::Uninitialized,)
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot be resumed")
    );

    for (state, expected) in [
        (ResidentSessionState::Running, "cannot overtake"),
        (
            ResidentSessionState::Unavailable,
            "cannot recreate live values or grants",
        ),
        (
            ResidentSessionState::Gone,
            "requires the original session incarnation",
        ),
    ] {
        let error = root_recovery_launch_mode(&binding, &failed, state)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{state:?}: {error}");
    }
}

#[test]
fn child_lifecycle_notice_contains_no_copied_runtime_state() {
    assert_eq!(
        CHILD_LIFECYCLE_NOTICE,
        "A child actor changed lifecycle state."
    );
    for forbidden in ["ActorRef", "ActorId", "incarnation", "handle", "summary"] {
        assert!(!CHILD_LIFECYCLE_NOTICE.contains(forbidden));
    }
}

#[test]
fn actor_workspace_recipes_distinguish_orchestrators_from_coding_workers() {
    let none = Vec::new();
    let one = vec!["worker-one".to_string()];
    let two = vec!["worker-one".to_string(), "worker-two".to_string()];

    assert_eq!(
        actor_workspace_request(true, &none),
        Ok(ActorWorkspaceRequest::SourceCheckout)
    );
    assert_eq!(
        actor_workspace_request(false, &none),
        Ok(ActorWorkspaceRequest::SourceCheckout)
    );
    assert_eq!(
        actor_workspace_request(false, &one),
        Ok(ActorWorkspaceRequest::Worktree("worker-one"))
    );
    assert!(actor_workspace_request(true, &one).is_err());
    assert!(actor_workspace_request(false, &two).is_err());

    let research = exomonad_actor::EffectiveRole::research();
    let instructions = developer_instructions(&research, &InteractiveLaunchMode::Fresh);
    assert!(instructions.starts_with(PromptId::ReadonlyAgent.body()));
    let normalized = normalized_prompt(&instructions);
    assert!(normalized.contains("Do not run builds, tests, formatters"));
    assert!(instructions.contains("native_tools=InspectionOnly"));
    assert!(instructions.contains(&research.haskell_effects_type()));

    let worker = developer_instructions(
        &exomonad_actor::EffectiveRole::coding(),
        &InteractiveLaunchMode::Fresh,
    );
    assert!(worker.starts_with(PromptId::WorktreeAgent.body()));
    let scaffold = developer_instructions(
        &exomonad_actor::EffectiveRole::scaffolding(exomonad_actor::DescendantBudget {
            maximum_depth: 2,
            maximum_active_children: Some(3),
        }),
        &InteractiveLaunchMode::Fork {
            parent: BackendThreadId("parent".into()),
            after_call: "call".into(),
        },
    );
    assert!(scaffold.starts_with(PromptId::ScaffoldingAgent.body()));
    assert!(scaffold.contains("descendant_depth=2; active_children=3"));
    assert!(!scaffold.contains("Some("), "{scaffold}");
    let integration = developer_instructions(
        &exomonad_actor::EffectiveRole::integration(),
        &InteractiveLaunchMode::Fresh,
    );
    assert!(integration.starts_with(PromptId::IntegrationAgent.body()));
    let root = developer_instructions(
        &exomonad_actor::EffectiveRole::root(),
        &InteractiveLaunchMode::Fresh,
    );
    assert!(root.contains("active_children=unbounded"), "{root}");
}

#[test]
fn root_and_worker_share_git_metadata_but_not_working_tree_authority() {
    let source = Path::new("/source");
    let worker = Path::new("/workers/one");
    let common = Path::new("/source/.git");

    assert_eq!(
        writable_repository_roots(
            true,
            exomonad_actor::WorkspaceAccess::WritableBound,
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
            exomonad_actor::WorkspaceAccess::WritableBound,
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
            exomonad_actor::WorkspaceAccess::InspectOnly,
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
        exomonad_actor::WorkspaceAccess::WritableBound,
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
            exomonad_actor::WorkspaceAccess::WritableBound,
            source,
            Some(&managed.join("wt-child")),
            common,
            Some(&root_worktrees),
        )
        .contains(&root_worktrees),
        "a child never receives the root's allocation directory"
    );
}

#[test]
fn worker_launch_unsets_source_checkout_extractor_pins() {
    let launch = actor_launch_environment(
        BTreeMap::from([
            ("PATH".into(), "/bin".into()),
            ("RUSTC_WRAPPER".into(), "/host/sccache".into()),
            ("RUSTC_WORKSPACE_WRAPPER".into(), "/host/wrapper".into()),
            ("TIDEPOOL_EXTRACT".into(), "/source/tidepool-extract".into()),
            (
                "TIDEPOOL_EXTRACT_WORKER".into(),
                "/source/tidepool-extract-worker".into(),
            ),
        ]),
        false,
        Some(Path::new(
            "/tmp/exomonad-actor-workspace/.exomonad/build/cargo",
        )),
    );
    assert_eq!(
        launch.unset,
        [
            "TIDEPOOL_EXTRACT".to_string(),
            "TIDEPOOL_EXTRACT_DAEMON_SOCKET".to_string(),
            "TIDEPOOL_EXTRACT_WORKER".to_string(),
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        launch.set.get("CARGO_TARGET_DIR").map(String::as_str),
        Some("/tmp/exomonad-actor-workspace/.exomonad/build/cargo")
    );
    assert_eq!(launch.set.get("PATH").map(String::as_str), Some("/bin"));
    assert_eq!(
        launch.set.get("RUSTC_WRAPPER").map(String::as_str),
        Some("")
    );
    assert_eq!(
        launch
            .set
            .get("RUSTC_WORKSPACE_WRAPPER")
            .map(String::as_str),
        Some("")
    );
    assert!(launch
        .unset
        .iter()
        .all(|name| !launch.set.contains_key(name)));
}

#[test]
fn root_launch_retains_its_source_checkout_toolchain() {
    let launch = actor_launch_environment(
        BTreeMap::from([("TIDEPOOL_EXTRACT".into(), "/source/extract".into())]),
        true,
        None,
    );
    assert!(launch.unset.is_empty());
    assert_eq!(
        launch.set.get("TIDEPOOL_EXTRACT").map(String::as_str),
        Some("/source/extract")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn actor_build_environment_overrides_cargo_config_wrappers_inside_mount() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let visible = root.path().join("visible");
    let resource = root.path().join("build-resource");
    let relative_target = Path::new(".exomonad/build/cargo");
    for path in [
        workspace.join("src"),
        workspace.join(".cargo"),
        workspace.join(relative_target),
        visible.join(relative_target),
        resource.clone(),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = \"actor-mount-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("src/lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
    .unwrap();
    std::fs::write(workspace.join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \"/host-only/compiler-wrapper\"\nrustc-workspace-wrapper = \"/host-only/workspace-wrapper\"\n").unwrap();
    let boundary = ProcessMountBoundary::new(&workspace, [workspace.clone()], [workspace.clone()])
        .unwrap()
        .with_project_root(&visible)
        .unwrap()
        .with_writable_overlay(&resource, visible.join(relative_target))
        .unwrap();
    let invocation = boundary.wrap(
        "bwrap",
        ProcessInvocation {
            program: "cargo".into(),
            args: vec!["check".into(), "--offline".into(), "--quiet".into()],
        },
    );
    let environment = actor_launch_environment(BTreeMap::new(), false, Some(relative_target));
    #[allow(
        clippy::disallowed_methods,
        reason = "test: short synchronous cargo check probe under a real mount boundary"
    )]
    let mut command = std::process::Command::new(invocation.program);
    command.args(invocation.args).envs(environment.set);
    for name in environment.unset {
        command.env_remove(name);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(resource.join("debug/deps").is_dir());
    assert!(!workspace.join(relative_target).join("debug").exists());
}

struct ScriptedPush {
    fail: std::sync::atomic::AtomicBool,
    messages: std::sync::Mutex<Vec<String>>,
}

#[test]
fn durable_actor_events_are_typed_and_legacy_rows_remain_readable() {
    let request = DurableActorEvent::Typed(TypedActorEvent::SessionReady {
        sequence: 4,
        request: exomonad_actor::RequestId(7),
        input_type: "Candidate".into(),
        message: "Review it.".into(),
    });
    let encoded = serde_json::to_value(&request).expect("serialize typed request event");
    assert_eq!(encoded["type"], "sessionReady");
    assert_eq!(encoded["request"], 7);
    assert_eq!(request.render(None), "Review it.");

    let watch = DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
        notification: exomonad_actor::WatchNotification {
            owner: exomonad_actor::ActorRef {
                id: exomonad_actor::ActorId(1),
                incarnation: exomonad_actor::Incarnation(1),
            },
            watch: exomonad_actor::WatchId(9),
            label: "join".into(),
            previous: exomonad_actor::WatchStateProjection::Pending,
            current: exomonad_actor::WatchStateProjection::Ready,
            transition: exomonad_actor::WatchTransition::Ready,
            occurred_at_unix_ms: 754_000,
            sequence: exomonad_actor::ActorEventSequence(3),
            watermark: exomonad_actor::ActorEventSequence(3),
        },
    });
    // The argument is the reading actor's launch time, so the text must not
    // claim to measure the actor the event is about.
    assert!(watch.render(Some(0)).contains("+12m34s into your session"));
    assert!(!watch.render(Some(0)).contains("since actor launch"));
    assert!(watch.render(None).contains("elapsed time unavailable"));
    assert!(watch
        .render(Some(800_000))
        .contains("elapsed time unavailable"));
    assert!(watch
        .render(None)
        .contains("cleanup may since have forgotten"));
    assert!(!watch.render(None).contains("pollWatch"));
    let encoded = serde_json::to_value(&watch).expect("serialize typed watch event");
    assert_eq!(encoded["type"], "watchChanged");
    assert_eq!(encoded["watch"], 9);

    let settlement = DurableActorEvent::Typed(TypedActorEvent::SettlementChanged {
        notification: exomonad_actor::SettlementNotification {
            owner: exomonad_actor::ActorRef {
                id: exomonad_actor::ActorId(1),
                incarnation: exomonad_actor::Incarnation(1),
            },
            request: exomonad_actor::RequestId(11),
            label: "implementation".into(),
            transition: exomonad_actor::SettlementTransition::Unavailable(
                exomonad_actor::ResponseFailure::TargetUnavailable,
            ),
            reply_preview: None,
            target_path: None,
            target_revision: None,
            occurred_at_unix_ms: 754_000,
            sequence: exomonad_actor::ActorEventSequence(4),
            watermark: exomonad_actor::ActorEventSequence(4),
        },
    });
    let rendered = settlement.render(Some(0));
    assert!(rendered.contains("request 11 \"implementation\" settled Unavailable"));
    assert!(rendered.contains("pollResponse"));
    let encoded = serde_json::to_value(&settlement).expect("serialize settlement event");
    assert_eq!(encoded["type"], "settlementChanged");
    assert_eq!(encoded["request"], 11);
    assert_eq!(
        serde_json::from_value::<DurableActorEvent>(serde_json::json!("old notice"))
            .expect("decode legacy actor event"),
        DurableActorEvent::Text("old notice".into())
    );

    // A `Ready` settlement carrying a reply preview puts the reply text
    // directly in the notice and softens the `pollResponse` guidance to an
    // optional follow-up, instead of insisting on it unconditionally.
    let settled_with_preview = DurableActorEvent::Typed(TypedActorEvent::SettlementChanged {
        notification: exomonad_actor::SettlementNotification {
            owner: exomonad_actor::ActorRef {
                id: exomonad_actor::ActorId(1),
                incarnation: exomonad_actor::Incarnation(1),
            },
            request: exomonad_actor::RequestId(12),
            label: "implementation".into(),
            transition: exomonad_actor::SettlementTransition::Ready,
            reply_preview: Some("\"looks correct, ship it\"".into()),
            target_path: None,
            target_revision: None,
            occurred_at_unix_ms: 754_000,
            sequence: exomonad_actor::ActorEventSequence(5),
            watermark: exomonad_actor::ActorEventSequence(5),
        },
    });
    let rendered = settled_with_preview.render(Some(0));
    assert!(rendered.contains("request 12 \"implementation\" settled Ready"));
    assert!(rendered.contains("Reply:\n\"looks correct, ship it\""));
    assert!(rendered.contains(
        "Read the full value with `pollResponse` only if you need more than this preview"
    ));
    assert!(!rendered.contains("Inspect its retained `Response` with `pollResponse`"));

    // A `Ready` settlement with no preview (observation failed, or the
    // reply's shape could not be read) keeps today's unconditional guidance.
    let settled_without_preview = DurableActorEvent::Typed(TypedActorEvent::SettlementChanged {
        notification: exomonad_actor::SettlementNotification {
            owner: exomonad_actor::ActorRef {
                id: exomonad_actor::ActorId(1),
                incarnation: exomonad_actor::Incarnation(1),
            },
            request: exomonad_actor::RequestId(13),
            label: "implementation".into(),
            transition: exomonad_actor::SettlementTransition::Ready,
            reply_preview: None,
            target_path: None,
            target_revision: None,
            occurred_at_unix_ms: 754_000,
            sequence: exomonad_actor::ActorEventSequence(6),
            watermark: exomonad_actor::ActorEventSequence(6),
        },
    });
    let rendered = settled_without_preview.render(Some(0));
    assert!(rendered.contains("request 13 \"implementation\" settled Ready"));
    assert!(rendered.contains(
        "Inspect its retained `Response` with `pollResponse`; settlement is not integration."
    ));
    assert!(!rendered.contains("Reply:\n"));

    // A preview `tidepool_runtime::ResidentSession::render_retained_preview`
    // cut for length carries its own truncation note; the notice renders it
    // exactly as given, alongside the softened `pollResponse` guidance.
    let long_preview = format!("{}\n[reply preview truncated]", "x".repeat(64));
    let settled_with_truncated_preview =
        DurableActorEvent::Typed(TypedActorEvent::SettlementChanged {
            notification: exomonad_actor::SettlementNotification {
                owner: exomonad_actor::ActorRef {
                    id: exomonad_actor::ActorId(1),
                    incarnation: exomonad_actor::Incarnation(1),
                },
                request: exomonad_actor::RequestId(14),
                label: "implementation".into(),
                transition: exomonad_actor::SettlementTransition::Ready,
                reply_preview: Some(long_preview.clone()),
                target_path: None,
                target_revision: None,
                occurred_at_unix_ms: 754_000,
                sequence: exomonad_actor::ActorEventSequence(7),
                watermark: exomonad_actor::ActorEventSequence(7),
            },
        });
    let rendered = settled_with_truncated_preview.render(Some(0));
    assert!(rendered.contains(&format!("Reply:\n{long_preview}")));
    assert!(rendered.contains("[reply preview truncated]"));
    assert!(rendered.contains(
        "Read the full value with `pollResponse` only if you need more than this preview"
    ));
}

/// A settlement notice for a child launched from a fork workspace names that
/// child on its own first line, ahead of the settlement body: its full
/// `exomonad/<path>` actor path, and the exact commit its checkout was
/// seeded from. A target with a path but no recorded seed revision (an
/// unforked launch, or a fork workspace admitted from a branch rather than a
/// captured OID) still gets the path line, just without a revision. A
/// target with no path at all (an unforked `startActor`/`startAgent`) keeps
/// today's rendering verbatim, with no leading identity line.
#[test]
fn settlement_notice_names_the_settled_child_on_its_first_line() {
    let owner = exomonad_actor::ActorRef {
        id: exomonad_actor::ActorId(1),
        incarnation: exomonad_actor::Incarnation(1),
    };
    let base = exomonad_actor::SettlementNotification {
        owner,
        request: exomonad_actor::RequestId(21),
        label: "correction".into(),
        transition: exomonad_actor::SettlementTransition::Ready,
        reply_preview: None,
        target_path: None,
        target_revision: None,
        occurred_at_unix_ms: 754_000,
        sequence: exomonad_actor::ActorEventSequence(8),
        watermark: exomonad_actor::ActorEventSequence(8),
    };

    let with_path_and_revision = DurableActorEvent::Typed(TypedActorEvent::SettlementChanged {
        notification: exomonad_actor::SettlementNotification {
            target_path: Some("correction-20260924/core-execution/core-correction".into()),
            target_revision: Some("a1b2c3d4e5f6".into()),
            ..base.clone()
        },
    });
    let rendered = with_path_and_revision.render(Some(0));
    let first_line = rendered
        .lines()
        .next()
        .expect("rendered notice has a first line");
    assert_eq!(
        first_line,
        "correction-20260924/core-execution/core-correction (seeded from a1b2c3d4e5f6)"
    );
    assert!(rendered.contains("request 21 \"correction\" settled Ready"));

    let with_path_only = DurableActorEvent::Typed(TypedActorEvent::SettlementChanged {
        notification: exomonad_actor::SettlementNotification {
            target_path: Some("correction-20260924/core-execution/core-correction".into()),
            target_revision: None,
            ..base.clone()
        },
    });
    let rendered = with_path_only.render(Some(0));
    let first_line = rendered
        .lines()
        .next()
        .expect("rendered notice has a first line");
    assert_eq!(
        first_line,
        "correction-20260924/core-execution/core-correction"
    );

    let without_identity =
        DurableActorEvent::Typed(TypedActorEvent::SettlementChanged { notification: base });
    let rendered = without_identity.render(Some(0));
    assert!(rendered.starts_with("request 21 \"correction\" settled Ready"));
}

#[tokio::test]
async fn queued_watch_forgotten_before_delivery_is_acknowledged_without_prompting() {
    use exomonad_agent::BackendThreadId;

    let root = tempfile::tempdir().unwrap();
    let rows = root.path().join("rows");
    let cursor = root.path().join("cursor");
    let inbox = Arc::new(ActorInbox::open(rows.clone(), cursor).unwrap());
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let notification = exomonad_actor::WatchNotification {
        owner: actor,
        watch: exomonad_actor::WatchId(19),
        label: "join".into(),
        previous: exomonad_actor::WatchStateProjection::Pending,
        current: exomonad_actor::WatchStateProjection::Ready,
        transition: exomonad_actor::WatchTransition::Ready,
        occurred_at_unix_ms: 0,
        sequence: exomonad_actor::ActorEventSequence(1),
        watermark: exomonad_actor::ActorEventSequence(1),
    };
    inbox
        .publish(DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
            notification: notification.clone(),
        }))
        .unwrap();
    let backend = ScriptedPush {
        fail: std::sync::atomic::AtomicBool::new(false),
        messages: std::sync::Mutex::new(Vec::new()),
    };
    let binding = root.path().join("binding.json");
    exomonad_agent::accept_interactive_session_binding(
        &binding,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into()),
        None,
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding)
        .await
        .unwrap();
    let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
    let (producer, reconciliations) = test_delivery_dependencies(root.path(), actor);

    deliver_pending_checked(
        actor,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
        &|owner, watch| owner != actor || watch != notification.watch,
        &|_, _, _| false,
    )
    .await
    .unwrap();

    assert!(backend.messages.lock().unwrap().is_empty());
    assert_eq!(inbox.cursor(), 1);
    assert_eq!(inbox.watermark(), 1);
    assert!(inbox.pending().unwrap().is_empty());
    assert!(std::fs::read_to_string(rows)
        .unwrap()
        .contains("watchChanged"));
}

/// A settlement notice queued behind a stuck native delivery must not wait on
/// it. `legacy_pending_prefix` only sees the contiguous untracked prefix, and
/// `deliver_tracked_message` only ever advances the one stuck sequence at the
/// front — so before this fix, a `SettlementChanged` row appended after a
/// tracked row stuck in `Unconfirmed`/`Submitted` was invisible to both and
/// never reached the model (matches actor 20 in run 535e56ca, inbox message 4
/// stuck `Submitted`). `deliver_out_of_order_notices` now surfaces it
/// directly, and does not re-render it once the barrier ahead clears.
#[tokio::test]
async fn settlement_notice_queued_behind_a_stuck_native_delivery_is_still_delivered() {
    use exomonad_agent::BackendThreadId;

    let root = tempfile::tempdir().unwrap();
    let rows = root.path().join("rows");
    let cursor = root.path().join("cursor");
    let inbox = Arc::new(ActorInbox::open(rows, cursor).unwrap());
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let sender = ActorRef::first(exomonad_actor::ActorId(8));

    // Sequence 1: a tracked native input row that will stay stuck.
    inbox
        .publish_tracked(
            DurableActorEvent::Text("stuck native input".into()),
            DeliveryProvenance::Notification {
                sender,
                target: actor,
            },
        )
        .unwrap();
    // Sequence 2: an untracked settlement notice queued behind it.
    let notification = exomonad_actor::SettlementNotification {
        owner: actor,
        request: exomonad_actor::RequestId(20),
        label: "child-20".into(),
        transition: exomonad_actor::SettlementTransition::Ready,
        reply_preview: Some("\"done\"".into()),
        target_path: None,
        target_revision: None,
        occurred_at_unix_ms: 0,
        sequence: exomonad_actor::ActorEventSequence(2),
        watermark: exomonad_actor::ActorEventSequence(2),
    };
    inbox
        .publish(DurableActorEvent::Typed(
            TypedActorEvent::SettlementChanged {
                notification: notification.clone(),
            },
        ))
        .unwrap();

    let backend = LostAckThenLate {
        late: exomonad_agent::InputAdmission::Unknown,
        submissions: std::sync::Mutex::new(Vec::new()),
        queries: std::sync::Mutex::new(Vec::new()),
        pushes: std::sync::Mutex::new(Vec::new()),
    };
    let binding = root.path().join("binding.json");
    exomonad_agent::accept_interactive_session_binding(
        &binding,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba3".into()),
        None,
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding)
        .await
        .unwrap();
    let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
    let (producer, reconciliations) = test_delivery_dependencies(root.path(), actor);

    // First tick: the front tracked row (Accepted) is submitted and comes
    // back `Unconfirmed`; delivery for it reports `Err`. The settlement
    // notice behind it must already have reached the backend this same
    // tick regardless.
    assert!(deliver_pending(
        actor,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .is_err());
    {
        let messages = backend.submissions.lock().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "the stuck row was attempted exactly once"
        );
    }
    let pushed_after_first_tick = backend.queries.lock().unwrap().clone();
    assert!(
        pushed_after_first_tick.is_empty(),
        "not queried until Unconfirmed"
    );
    // The barrier still holds: nothing is acknowledged yet.
    assert_eq!(inbox.cursor(), 0);

    // Second tick: the front row is now `Unconfirmed`, queried and stays
    // stuck (`Unknown`). The already-surfaced settlement notice must not be
    // pushed to the backend a second time.
    assert!(deliver_pending(
        actor,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .is_err());
    assert_eq!(backend.queries.lock().unwrap().len(), 1);
    assert_eq!(
        inbox.cursor(),
        0,
        "the stuck row still fences acknowledgement"
    );

    // The rendered settlement text reached the backend out of order, even
    // though the stuck native row ahead of it never resolved — and exactly
    // once, not once per stuck tick.
    let rendered = DurableActorEvent::Typed(TypedActorEvent::SettlementChanged {
        notification: notification.clone(),
    })
    .render(None);
    let pushes = backend.pushes.lock().unwrap();
    assert_eq!(
        pushes.iter().filter(|message| message.contains("request 20")).count(),
        1,
        "settlement notice for request 20 must reach the backend exactly once despite the stuck row ahead of it: {pushes:?}"
    );
    assert!(pushes.contains(&rendered));
}

#[tokio::test]
async fn queued_watch_notice_already_observed_by_the_owner_is_acknowledged_without_prompting() {
    use exomonad_agent::BackendThreadId;

    let root = tempfile::tempdir().unwrap();
    let rows = root.path().join("rows");
    let cursor = root.path().join("cursor");
    let inbox = Arc::new(ActorInbox::open(rows.clone(), cursor).unwrap());
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let notification = exomonad_actor::WatchNotification {
        owner: actor,
        watch: exomonad_actor::WatchId(19),
        label: "join".into(),
        previous: exomonad_actor::WatchStateProjection::Pending,
        current: exomonad_actor::WatchStateProjection::Ready,
        transition: exomonad_actor::WatchTransition::Ready,
        occurred_at_unix_ms: 1_000,
        sequence: exomonad_actor::ActorEventSequence(1),
        watermark: exomonad_actor::ActorEventSequence(1),
    };
    inbox
        .publish(DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
            notification: notification.clone(),
        }))
        .unwrap();
    let backend = ScriptedPush {
        fail: std::sync::atomic::AtomicBool::new(false),
        messages: std::sync::Mutex::new(Vec::new()),
    };
    let binding = root.path().join("binding.json");
    exomonad_agent::accept_interactive_session_binding(
        &binding,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba2".into()),
        None,
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding)
        .await
        .unwrap();
    let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
    let (producer, reconciliations) = test_delivery_dependencies(root.path(), actor);

    // The owner observed this exact watch settle Ready at 2_000ms, strictly
    // after the notice queued for the transition at 1_000ms. The notice is
    // stale by the time it would be delivered: acknowledge it silently.
    deliver_pending_checked(
        actor,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
        &|_, _| true,
        &|owner, watch, occurred_at_unix_ms| {
            owner == actor && watch == notification.watch && occurred_at_unix_ms <= 2_000
        },
    )
    .await
    .unwrap();

    assert!(backend.messages.lock().unwrap().is_empty());
    assert_eq!(inbox.cursor(), 1);
    assert_eq!(inbox.watermark(), 1);
    assert!(inbox.pending().unwrap().is_empty());
}

#[tokio::test]
async fn queued_watch_notice_observed_before_the_transition_is_not_suppressed() {
    use exomonad_agent::BackendThreadId;

    let root = tempfile::tempdir().unwrap();
    let rows = root.path().join("rows");
    let cursor = root.path().join("cursor");
    let inbox = Arc::new(ActorInbox::open(rows.clone(), cursor).unwrap());
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let notification = exomonad_actor::WatchNotification {
        owner: actor,
        watch: exomonad_actor::WatchId(19),
        label: "join".into(),
        previous: exomonad_actor::WatchStateProjection::Pending,
        current: exomonad_actor::WatchStateProjection::Ready,
        transition: exomonad_actor::WatchTransition::Ready,
        occurred_at_unix_ms: 1_000,
        sequence: exomonad_actor::ActorEventSequence(1),
        watermark: exomonad_actor::ActorEventSequence(1),
    };
    inbox
        .publish(DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
            notification: notification.clone(),
        }))
        .unwrap();
    let backend = ScriptedPush {
        fail: std::sync::atomic::AtomicBool::new(false),
        messages: std::sync::Mutex::new(Vec::new()),
    };
    let binding = root.path().join("binding.json");
    exomonad_agent::accept_interactive_session_binding(
        &binding,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba3".into()),
        None,
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding)
        .await
        .unwrap();
    let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
    let (producer, reconciliations) = test_delivery_dependencies(root.path(), actor);

    // The owner had observed this watch earlier, at 500ms, still Pending: an
    // observation made before the notice's transition (at 1_000ms) says
    // nothing about the state the notice describes and must not suppress
    // delivery of the notice, which prompts as usual.
    deliver_pending_checked(
        actor,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
        &|_, _| true,
        &|owner, watch, occurred_at_unix_ms| {
            owner == actor && watch == notification.watch && occurred_at_unix_ms <= 500
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.messages.lock().unwrap().len(), 1);
    assert_eq!(inbox.cursor(), 1);
    assert_eq!(inbox.watermark(), 1);
}

impl InteractiveAgentBackend for ScriptedPush {
    fn prepare_native_tool_policy(
        &self,
        _policy: InteractiveNativeToolPolicy,
        _staging_root: &Path,
    ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
        Ok(Vec::new())
    }

    fn render(
        &self,
        _spec: &InteractiveAgentSpec,
    ) -> Result<InteractiveAgentCommand, AgentBackendError> {
        Err(AgentBackendError::ProtocolRejected {
            detail: "render is outside this delivery test".into(),
        })
    }

    fn push<'a>(
        &'a self,
        _cwd: &'a str,
        _thread: &'a QueueReadyThread,
        message: &'a str,
    ) -> InteractiveFuture<'a, ()> {
        Box::pin(async move {
            self.messages.lock().unwrap().push(message.into());
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                Err(AgentBackendError::BackendUnavailable {
                    detail: "temporary push failure".into(),
                })
            } else {
                Ok(())
            }
        })
    }

    fn archive<'a>(
        &'a self,
        _cwd: &'a str,
        _thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

struct ScriptedSteering(ScriptedPush);

impl InteractiveAgentBackend for ScriptedSteering {
    fn submit_input<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        envelope: &'a InteractiveInputEnvelope,
    ) -> exomonad_agent::InteractiveInputFuture<'a> {
        Box::pin(async move {
            self.0.messages.lock().unwrap().push(format!(
                "{}:{}",
                envelope.id().native_key(),
                String::from_utf8_lossy(envelope.bytes())
            ));
            if self.0.fail.load(std::sync::atomic::Ordering::SeqCst) {
                Err(exomonad_agent::InteractiveInputError::Unconfirmed(
                    "lost confirmation".into(),
                ))
            } else {
                Ok(exomonad_agent::InputAdmission::Presented)
            }
        })
    }

    fn query_input<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _id: &'a InputOperationId,
    ) -> exomonad_agent::InteractiveInputFuture<'a> {
        Box::pin(async { Ok(exomonad_agent::InputAdmission::Unknown) })
    }

    fn prepare_native_tool_policy(
        &self,
        policy: InteractiveNativeToolPolicy,
        root: &Path,
    ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
        self.0.prepare_native_tool_policy(policy, root)
    }
    fn render(
        &self,
        spec: &InteractiveAgentSpec,
    ) -> Result<InteractiveAgentCommand, AgentBackendError> {
        self.0.render(spec)
    }
    fn push<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a QueueReadyThread,
        message: &'a str,
    ) -> InteractiveFuture<'a, ()> {
        self.0.push(cwd, thread, message)
    }
    fn archive<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, ()> {
        self.0.archive(cwd, thread)
    }
}

struct LostAckThenLate {
    late: exomonad_agent::InputAdmission,
    submissions: std::sync::Mutex<Vec<String>>,
    queries: std::sync::Mutex<Vec<String>>,
    pushes: std::sync::Mutex<Vec<String>>,
}

impl InteractiveAgentBackend for LostAckThenLate {
    fn prepare_native_tool_policy(
        &self,
        _policy: InteractiveNativeToolPolicy,
        _root: &Path,
    ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
        Ok(Vec::new())
    }

    fn render(
        &self,
        _spec: &InteractiveAgentSpec,
    ) -> Result<InteractiveAgentCommand, AgentBackendError> {
        Err(AgentBackendError::ProtocolRejected {
            detail: "render is outside this delivery test".into(),
        })
    }

    fn push<'a>(
        &'a self,
        _cwd: &'a str,
        _thread: &'a QueueReadyThread,
        message: &'a str,
    ) -> InteractiveFuture<'a, ()> {
        Box::pin(async move {
            self.pushes.lock().unwrap().push(message.into());
            Ok(())
        })
    }

    fn archive<'a>(
        &'a self,
        _cwd: &'a str,
        _thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn submit_input<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        envelope: &'a InteractiveInputEnvelope,
    ) -> exomonad_agent::InteractiveInputFuture<'a> {
        Box::pin(async move {
            self.submissions
                .lock()
                .unwrap()
                .push(envelope.id().native_key());
            Err(exomonad_agent::InteractiveInputError::Unconfirmed(
                "native accepted input but its reply was lost".into(),
            ))
        })
    }

    fn query_input<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        id: &'a InputOperationId,
    ) -> exomonad_agent::InteractiveInputFuture<'a> {
        Box::pin(async move {
            self.queries.lock().unwrap().push(id.native_key());
            Ok(self.late)
        })
    }
}

#[tokio::test]
async fn tracked_steering_preserves_order_and_never_retries_uncertain_input() {
    use exomonad_node::{DeliveryPhase, ReceiptLookup};
    for uncertain in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let inbox = Arc::new(
            ActorInbox::open(root.path().join("rows"), root.path().join("cursor")).unwrap(),
        );
        let actor = ActorRef::first(exomonad_actor::ActorId(7));
        for message in ["A", "B"] {
            inbox
                .publish_tracked(
                    DurableActorEvent::Text(message.into()),
                    DeliveryProvenance::Notification {
                        sender: ActorRef::first(exomonad_actor::ActorId(8)),
                        target: actor,
                    },
                )
                .unwrap();
        }
        let backend = ScriptedSteering(ScriptedPush {
            fail: std::sync::atomic::AtomicBool::new(uncertain),
            messages: std::sync::Mutex::new(Vec::new()),
        });
        let binding = root.path().join("binding.json");
        exomonad_agent::accept_interactive_session_binding(
            &binding,
            exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
            BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into()),
            None,
        )
        .await
        .unwrap();
        let thread = exomonad_agent::read_interactive_binding(&binding)
            .await
            .unwrap();
        let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
        let (producer, reconciliations) = test_delivery_dependencies(root.path(), actor);
        for _ in 0..3 {
            let outcome = deliver_pending(
                actor,
                &inbox,
                &thread,
                &backend,
                &producer,
                &reconciliations,
                root.path(),
                &observation,
            )
            .await;
            assert_eq!(outcome.is_err(), uncertain);
        }
        let messages = backend.0.messages.lock().unwrap();
        if uncertain {
            let first = format!(
                "{}:A",
                InputOperationId {
                    producer: producer.clone(),
                    sequence: std::num::NonZeroU64::new(1).unwrap(),
                }
                .native_key()
            );
            assert_eq!(&*messages, &[first]);
            assert_eq!(inbox.cursor(), 0);
            assert!(
                matches!(inbox.observe_receipt(1).unwrap(), ReceiptLookup::Retained(e) if e.phase == DeliveryPhase::Unconfirmed)
            );
            assert!(
                matches!(inbox.observe_receipt(2).unwrap(), ReceiptLookup::Retained(e) if e.phase == DeliveryPhase::Accepted)
            );
        } else {
            let expected = [1_u64, 2]
                .into_iter()
                .zip(["A", "B"])
                .map(|(sequence, message)| {
                    format!(
                        "{}:{message}",
                        InputOperationId {
                            producer: producer.clone(),
                            sequence: std::num::NonZeroU64::new(sequence).unwrap(),
                        }
                        .native_key()
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(&*messages, &expected);
            assert_eq!(inbox.cursor(), 2);
            for sequence in [1, 2] {
                assert!(
                    matches!(inbox.observe_receipt(sequence).unwrap(), ReceiptLookup::Retained(e) if e.phase == DeliveryPhase::Presented)
                );
            }
        }
    }
}

#[tokio::test]
async fn restart_queries_exact_lost_ack_and_late_compacted_permanently_fences_successor() {
    use exomonad_node::{DeliveryPhase, ReceiptLookup};
    let root = tempfile::tempdir().unwrap();
    let rows = root.path().join("rows");
    let cursor = root.path().join("cursor");
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let sender = ActorRef::first(exomonad_actor::ActorId(8));
    let inbox = Arc::new(ActorInbox::open(rows.clone(), cursor.clone()).unwrap());
    for message in ["lost ack", "later"] {
        inbox
            .publish_tracked(
                DurableActorEvent::Text(message.into()),
                DeliveryProvenance::Notification {
                    sender,
                    target: actor,
                },
            )
            .unwrap();
    }
    let binding = root.path().join("binding.json");
    exomonad_agent::accept_interactive_session_binding(
        &binding,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into()),
        None,
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding)
        .await
        .unwrap();
    let backend = LostAckThenLate {
        late: exomonad_agent::InputAdmission::Compacted,
        submissions: std::sync::Mutex::new(Vec::new()),
        queries: std::sync::Mutex::new(Vec::new()),
        pushes: std::sync::Mutex::new(Vec::new()),
    };
    let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
    let (producer, reconciliations) = test_delivery_dependencies(root.path(), actor);

    assert!(deliver_pending(
        actor,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .is_err());
    assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    assert!(backend.queries.lock().unwrap().is_empty());
    assert!(
        matches!(inbox.observe_receipt(1).unwrap(), ReceiptLookup::Retained(e) if e.phase == DeliveryPhase::Unconfirmed)
    );
    drop(inbox);

    let reopened = Arc::new(ActorInbox::open(rows, cursor).unwrap());
    assert!(deliver_pending(
        actor,
        &reopened,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .is_err());
    assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    assert_eq!(backend.queries.lock().unwrap().len(), 1);
    assert!(
        matches!(reopened.observe_receipt(1).unwrap(), ReceiptLookup::Retained(e) if e.phase == DeliveryPhase::Compacted)
    );
    assert!(
        matches!(reopened.observe_receipt(2).unwrap(), ReceiptLookup::Retained(e) if e.phase == DeliveryPhase::Accepted)
    );
    assert_eq!(reopened.cursor(), 0);

    assert!(deliver_pending(
        actor,
        &reopened,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .is_err());
    assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    assert_eq!(backend.queries.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn production_native_socket_lost_ack_restarts_as_exact_query_without_overtaking() {
    use exomonad_agent::{InputAdmission, InteractiveSessionBinding};
    use exomonad_node::{DeliveryPhase, ReceiptLookup};
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_request(stream: &mut tokio::net::UnixStream) -> serde_json::Value {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0_u8; 4096];
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
            let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") else {
                continue;
            };
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            let length: usize = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }

    async fn reply(
        stream: &mut tokio::net::UnixStream,
        binding: &InteractiveSessionBinding,
        outcome: &str,
    ) {
        let body = serde_json::to_vec(&serde_json::json!({
            "binding": {
                "protocolVersion": codex_shoal_protocol::INPUT_CONTROL_PROTOCOL_VERSION,
                "launchId": binding.launch_id,
                "instanceId": binding.instance_id,
                "generation": binding.generation.get(),
                "nonce": binding.nonce,
            },
            "outcome": outcome,
        }))
        .unwrap();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&body).await.unwrap();
    }

    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("native-input.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let binding = InteractiveSessionBinding {
        launch_id: "launch-host-test".into(),
        instance_id: "instance-host-test".into(),
        generation: std::num::NonZeroU64::new(7).unwrap(),
        nonce: "nonce-host-test".into(),
    };
    let server_binding = binding.clone();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for index in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            requests.push(read_request(&mut stream).await);
            match index {
                0 => reply(&mut stream, &server_binding, "admitted").await,
                1 => drop(stream),
                2 => reply(&mut stream, &server_binding, "compacted").await,
                _ => unreachable!(),
            }
        }
        requests
    });
    let binding_path = root.path().join("binding.json");
    let thread_id = BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into());
    exomonad_agent::accept_interactive_session_binding(
        &binding_path,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        thread_id,
        Some(socket),
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding_path)
        .await
        .unwrap()
        .with_challenged_session_binding(Some(binding));
    let executable = root.path().join("codex-test");
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let installation =
        exomonad_agent::native_interactive_agent_from_parts(executable, "codex-test".into())
            .unwrap();
    let backend = exomonad_agent::native_interactive_backend(installation);
    assert_eq!(
        backend.bind_input(&thread).await.unwrap(),
        InputAdmission::Admitted
    );

    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let sender = ActorRef::first(exomonad_actor::ActorId(8));
    let rows = root.path().join("rows");
    let cursor = root.path().join("cursor");
    let inbox = Arc::new(ActorInbox::open(rows.clone(), cursor.clone()).unwrap());
    for message in ["same immutable operation", "later"] {
        inbox
            .publish_tracked(
                DurableActorEvent::Text(message.into()),
                DeliveryProvenance::Notification {
                    sender,
                    target: actor,
                },
            )
            .unwrap();
    }
    let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
    let (producer, reconciliations) = test_delivery_dependencies(root.path(), actor);
    assert!(deliver_pending(
        actor,
        &inbox,
        &thread,
        backend.as_ref(),
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .is_err());
    assert!(
        matches!(inbox.observe_receipt(1).unwrap(), ReceiptLookup::Retained(e) if e.phase == DeliveryPhase::Unconfirmed)
    );
    drop(inbox);

    let reopened = Arc::new(ActorInbox::open(rows, cursor).unwrap());
    assert!(deliver_pending(
        actor,
        &reopened,
        &thread,
        backend.as_ref(),
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .is_err());
    assert!(
        matches!(reopened.observe_receipt(1).unwrap(), ReceiptLookup::Retained(e) if e.phase == DeliveryPhase::Compacted)
    );
    assert!(
        matches!(reopened.observe_receipt(2).unwrap(), ReceiptLookup::Retained(e) if e.phase == DeliveryPhase::Accepted)
    );
    assert_eq!(reopened.cursor(), 0);

    let requests = server.await.unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["operation"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["bind", "submit", "query"]
    );
    assert_eq!(requests[1]["envelope"]["producerId"], producer.as_str());
    assert_eq!(requests[1]["envelope"]["sequence"], 1);
    assert_eq!(requests[2]["producer_id"], producer.as_str());
    assert_eq!(requests[2]["sequence"], 1);
}

#[tokio::test]
async fn launch_shutdown_preserves_socket_error_and_completed_results() {
    let root = tempfile::tempdir().unwrap();
    let actor = ActorRef::first(exomonad_actor::ActorId(80));
    let successful_path = root.path().join("successful");
    let failed_path = root.path().join("failed");
    let mut successful = SocketDirectory::create(successful_path.clone()).unwrap();
    successful.work_may_exist();
    let mut failed = SocketDirectory::create(failed_path.clone()).unwrap();
    failed.work_may_exist();
    let error = socket_launch_failure(
        actor,
        InteractiveOperation::LaunchProcess,
        "launch cancelled after hosted work submission",
        failed,
    );
    let mut launches = JoinSet::new();
    launches.spawn(async move { ((), Ok(Some(successful))) });
    launches.spawn(async move { ((), Err(error)) });
    launches.spawn(async { ((), Ok(None)) });
    let outcome = drain_launches_for_shutdown(&mut launches, Duration::from_secs(1)).await;
    assert_eq!(outcome.completed.len(), 1);
    assert_eq!(outcome.completed[0].path(), successful_path);
    assert!(
        matches!(outcome.failures.as_slice(), [LaunchShutdownFailure::Launch(error)]
        if error.actor == actor && error.detail.contains("socket cleanup failed:") && error.detail.contains("unconfirmed"))
    );
    assert!(failed_path.exists());
    assert!(launches.is_empty());
    // This is the same value the production caller transfers into retirement.
    assert!(matches!(
        socket_cleanup_outcome(outcome.completed.into_iter().next().unwrap()),
        CleanupComponentOutcome::Failed { .. }
    ));
    assert!(successful_path.exists());
}

#[tokio::test]
async fn launch_shutdown_retains_join_failure_without_a_deployment() {
    let mut launches: JoinSet<((), Result<Option<()>, InteractiveApplicationError>)> =
        JoinSet::new();
    launches.spawn(async { panic!("launch task panicked before result") });
    let outcome = drain_launches_for_shutdown(&mut launches, Duration::from_secs(1)).await;
    assert!(outcome.completed.is_empty());
    assert!(
        matches!(outcome.failures.as_slice(), [LaunchShutdownFailure::Join(error)] if error.is_panic())
    );
}

#[tokio::test]
async fn launch_shutdown_timeout_preserves_partial_success_and_reports_uncertain_abort() {
    let root = tempfile::tempdir().unwrap();
    let ready_path = root.path().join("ready");
    let pending_path = root.path().join("pending");
    let mut ready = SocketDirectory::create(ready_path.clone()).unwrap();
    ready.work_may_exist();
    let mut pending = SocketDirectory::create(pending_path.clone()).unwrap();
    pending.work_may_exist();
    let mut launches = JoinSet::new();
    launches.spawn(async move { ((), Ok(Some(ready))) });
    launches.spawn(async move {
        let _retained = pending;
        std::future::pending::<(
            (),
            Result<Option<SocketDirectory>, InteractiveApplicationError>,
        )>()
        .await
    });
    let outcome = drain_launches_for_shutdown(&mut launches, Duration::from_millis(100)).await;
    assert_eq!(outcome.completed.len(), 1);
    assert_eq!(outcome.completed[0].path(), ready_path);
    assert!(outcome
        .failures
        .iter()
        .any(|failure| matches!(failure, LaunchShutdownFailure::TimedOut { pending: 1 })));
    // Confirm the test task stops, without upgrading the recorded uncertainty.
    while !launches.is_empty() {
        let result = tokio::time::timeout(Duration::from_secs(1), launches.join_next())
            .await
            .unwrap()
            .unwrap();
        assert!(result.unwrap_err().is_cancelled());
    }
    assert!(pending_path.exists());
    assert!(ready_path.exists());
    assert!(!outcome.failures.is_empty());
}

#[tokio::test]
async fn socket_preparation_cleans_failed_inbox_open_and_preserves_collision() {
    let root = tempfile::tempdir().unwrap();
    let actor = ActorRef::first(exomonad_actor::ActorId(77));
    let socket_path = root.path().join("socket");
    let rows = root.path().join("rows");
    let cursor = root.path().join("cursor");
    std::fs::write(&cursor, b"not a checkpoint").unwrap();
    let error = prepare_socket_inbox(actor, socket_path.clone(), rows.clone(), cursor.clone())
        .err()
        .expect("invalid inbox checkpoint must fail preparation");
    assert!(matches!(
        error.operation,
        InteractiveOperation::PrepareRuntime
    ));
    assert!(error.detail.contains("expected"), "{error}");
    // BindToolHost succeeded before the malformed checkpoint was read. Its
    // named socket and exclusively created parent must both be removed.
    assert!(!socket_path.join("host-tools.sock").exists());
    assert!(!socket_path.exists());
    assert_eq!(std::fs::read(&cursor).unwrap(), b"not a checkpoint");

    std::fs::create_dir(&socket_path).unwrap();
    let preexisting = UnixListener::bind(socket_path.join("host-tools.sock")).unwrap();
    std::fs::write(socket_path.join("marker"), b"belongs to another owner").unwrap();
    assert!(prepare_socket_inbox(actor, socket_path.clone(), rows, cursor).is_err());
    assert!(socket_path.join("host-tools.sock").exists());
    assert_eq!(
        std::fs::read(socket_path.join("marker")).unwrap(),
        b"belongs to another owner"
    );
    drop(preexisting);
}

#[tokio::test]
async fn socket_preparation_cleans_bind_failure() {
    let root = tempfile::tempdir().unwrap();
    // Valid directory component, but longer than Unix socket sockaddr paths.
    let path = root.path().join("s".repeat(150));
    let error = prepare_socket_inbox(
        ActorRef::first(exomonad_actor::ActorId(79)),
        path.clone(),
        root.path().join("rows"),
        root.path().join("cursor"),
    )
    .err()
    .expect("overlong socket endpoint must fail to bind");
    assert!(matches!(
        error.operation,
        InteractiveOperation::BindToolHost
    ));
    assert!(!path.exists());
    assert!(!root.path().join("rows").exists());
}

#[tokio::test]
async fn socket_postsubmission_error_and_retirement_report_retention() {
    let root = tempfile::tempdir().unwrap();
    let actor = ActorRef::first(exomonad_actor::ActorId(78));
    for retire in [false, true] {
        let path = root.path().join(if retire { "retire" } else { "launch" });
        let (mut socket, listener, _) = prepare_socket_inbox(
            actor,
            path.clone(),
            root.path().join("rows"),
            root.path().join("cursor"),
        )
        .unwrap();
        socket.work_may_exist();
        // Dropping/aborting the listener does not prove accepted hosted work
        // or a native process is finished.
        drop(listener);
        if retire {
            assert!(matches!(socket_cleanup_outcome(socket),
                CleanupComponentOutcome::Failed { detail } if detail.contains("unconfirmed")));
        } else {
            let error = socket_launch_failure(
                actor,
                InteractiveOperation::LaunchProcess,
                "launch timed out",
                socket,
            );
            assert!(error
                .detail
                .contains("launch timed out; socket cleanup failed:"));
            assert!(error.detail.contains("unconfirmed"));
        }
        assert!(path.join("host-tools.sock").exists());
    }
}

#[tokio::test]
async fn notification_admission_and_poll_preserve_typed_request_bindings() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(root.as_ref(), include_str!("notification_setup.hs")).await;
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
        directory.path().join("rows-tree/deep/rows"),
        directory.path().join("checkpoint-tree/deep/cursor"),
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
    let policy = root.clone();
    let poll = tokio::spawn(async move {
        dispatch_haskell_script(policy.as_ref(), "pollNotification receipt").await
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
        foreign_directory.path().join("rows-tree/deep/rows"),
        foreign_directory.path().join("checkpoint-tree/deep/cursor"),
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
        directory.path().join("rows-tree/deep/rows"),
        directory.path().join("checkpoint-tree/deep/cursor"),
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
    let observed = poll.await.unwrap();
    assert_eq!(observed["status"], "committed", "{observed:?}");
    assert!(
        observed.to_string().contains("NotificationAccepted"),
        "{observed:?}"
    );
    let unchanged =
        dispatch_haskell_script(child.policy.as_ref(), "inspectFull sessionInput").await;
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
    let answer =
        dispatch_haskell_script(root.as_ref(), "inspectFull <$> pollResponse answer").await;
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
        "idle <- startAgent (readonlyAgent \"idle-notification-recipient\")",
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
        idle_directory.path().join("rows-tree/deep/rows"),
        idle_directory.path().join("checkpoint-tree/deep/cursor"),
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
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
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
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(root.as_ref(), include_str!("notification_setup.hs")).await;
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
    let inbox = ActorInbox::open(
        directory.path().join("rows-tree/deep/rows"),
        directory.path().join("checkpoint-tree/deep/cursor"),
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
        .next_deployment(
            "notification send",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationSend(command) => Ok(command),
                other => Err(other),
            },
        )
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
    let reply =
        dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput :: Text)").await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    let answer =
        dispatch_haskell_script(root.as_ref(), "inspectFull <$> pollResponse answer").await;
    assert_eq!(answer["status"], "committed", "{answer:?}");
    assert!(
        answer.to_string().contains("original assignment"),
        "{answer:?}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// Companion to [`held_native_delivery_preserves_typed_request_bindings`]:
/// `lookup` selects the same request-aware workbench `respond` cells do,
/// and the extractor resolves the preamble's own declarations (where
/// `respond`/`sessionReply`/`sessionInput` are bound) through the query
/// module's typechecked environment, since that module is never loaded and
/// `getInfo` alone cannot see its top level.
#[tokio::test]
async fn lookup_during_held_native_delivery_returns_respond_signature() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(root.as_ref(), include_str!("notification_setup.hs")).await;
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
                LocalResidentDeployment::SessionReady { activation } => Ok(activation),
                other => Err(other),
            },
        )
        .await;
    let directory = tempfile::tempdir().unwrap();
    let inbox = ActorInbox::open(
        directory.path().join("rows-tree/deep/rows"),
        directory.path().join("checkpoint-tree/deep/cursor"),
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
        .next_deployment(
            "notification send",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationSend(command) => Ok(command),
                other => Err(other),
            },
        )
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
    let reply =
        dispatch_haskell_script(child.policy.as_ref(), "respond (sessionInput :: Text)").await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A settlement notice's reply preview is read directly off the heap
/// (`ResidentSession::render_retained_preview`), no Haskell compiled and no
/// `pollResponse` cell needed. It reads constructor-shaped values -- an
/// ordinary Haskell `String`/`[Char]` among them. The reply carries a
/// `WorkbenchDisplay`-rendered preview from the Haskell side
/// (`Tidepool.Agent.Reply.Internal.reply`) precisely so `Data.Text.Text`
/// fields -- a packed byte array this session's own non-forcing heap walk
/// cannot see into -- are readable too; see the companion `..._for_text`
/// test below.
#[tokio::test]
async fn settlement_notice_carries_a_readable_reply_preview() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        "worker <- startAgent (readonlyAgent \"reply-preview-recipient\")\n\
         let requestName = [label|reply-preview|]\n\
         answer <- request @String worker (assignment requestName (\"a readable reply\" :: String))",
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
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// [`settlement_notice_carries_a_readable_reply_preview`], but the reply
/// type is `Data.Text.Text`: a packed byte array the host's own non-forcing
/// retained-heap walk cannot read (it would print `…`). The preview still
/// carries the literal text because `Tidepool.Agent.Reply.Internal.reply`
/// renders it on the Haskell side (`WorkbenchDisplay`) before it ever
/// reaches the host.
#[tokio::test]
async fn settlement_notice_carries_a_readable_reply_preview_for_text() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        "worker <- startAgent (readonlyAgent \"reply-preview-text-recipient\")\n\
         let requestName = [label|reply-preview-text|]\n\
         answer <- request @Text worker (assignment requestName (\"a readable reply\" :: Text))",
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
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn record_actor_dispatches_typed_routes_and_commits_state() {
    let campaign = test_campaign::TestCampaign::start().await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        include_str!("record_actor.hs"),
    )
    .await;
    assert_eq!(result["status"], "committed", "{result:?}");
    for item in result["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{result:?}");
    }
    assert!(result.to_string().contains("True"), "{result:?}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn root_journal_effect_appends_a_typed_record() {
    let campaign = test_campaign::TestCampaign::start().await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        include_str!("journal_record.hs"),
    )
    .await;
    assert_eq!(result["status"], "committed", "{result:?}");
    let run_id = campaign
        .session_root
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("model-free run root has a UTF-8 test id");
    let journal_path = crate::exomonad::exomonad_journal_path(campaign._repository.path(), run_id);
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
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn record_actor_sleep_keeps_mailbox_handlers_sequential() {
    let campaign = test_campaign::TestCampaign::start().await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        include_str!("record_actor_sleep.hs"),
    )
    .await;
    assert_eq!(result["status"], "committed", "{result:?}");
    for item in result["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{result:?}");
    }
    assert!(result.to_string().contains("True"), "{result:?}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn record_actor_nested_failure_reaches_interactive_owner_once() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        include_str!("record_actor_nested_failure.hs"),
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
    let available =
        dispatch_haskell_script(root.as_ref(), "R.call (managerValue (R.client manager)) ()").await;
    assert_eq!(available["status"], "committed", "{available:?}");
    assert_eq!(available["items"][0]["output"], "7", "{available:?}");
    campaign.assert_no_deployment("duplicate failure notice", |_| true);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn record_actor_explains_invalid_state_shapes() {
    let campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        include_str!("record_actor_invalid_shapes.hs"),
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
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn stateful_actor_drains_accepted_messages_into_its_retained_exit() {
    let campaign = test_campaign::TestCampaign::start().await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        include_str!("stateful_drain.hs"),
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
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn stateful_handler_failure_after_effect_pauses_without_replay_or_closing_mailbox() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(root.as_ref(), include_str!("stateful_failure.hs")).await;
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
    let repaired =
        dispatch_haskell_script(root.as_ref(), include_str!("stateful_replacement.hs")).await;
    assert_eq!(repaired["status"], "committed", "{repaired:?}");
    for item in repaired["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{repaired:?}");
    }
    assert!(!repaired.to_string().contains("False"), "{repaired:?}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn lifecycle_sources_follow_replacement_and_capture_retained_exit() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    for (stage, source) in include_str!("lifecycle_source.hs")
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
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn stateful_replacement_rejects_changed_state_and_protocol_types() {
    let campaign = test_campaign::TestCampaign::start().await;
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
        assert!(!diagnostic.contains("Variable not in scope"), "{diagnostic}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn stateful_replacement_preserves_owned_children() {
    let campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let ownership =
        dispatch_haskell_script(root.as_ref(), include_str!("stateful_replacement_tree.hs")).await;
    assert_eq!(ownership["status"], "committed", "{ownership:?}");
    for item in ownership["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{ownership:?}");
    }
    assert!(
        ownership.to_string().contains("True") && !ownership.to_string().contains("False"),
        "{ownership:?}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn haskell_mailbox_preserves_state_and_opaque_replies_across_calls() {
    let campaign = test_campaign::TestCampaign::start().await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        include_str!("mailbox_state.hs"),
    )
    .await;
    assert_eq!(result["status"], "committed", "{result:?}");
    for item in result["items"].as_array().unwrap() {
        assert_eq!(item["status"], "committed", "{result:?}");
    }
    assert!(result.to_string().contains("True"), "{result:?}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn haskell_actor_sends_normal_steering_without_a_native_session() {
    let mut campaign = test_campaign::TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let policy = root.clone();
    let mut run = tokio::spawn(async move {
        dispatch_haskell_script(policy.as_ref(), include_str!("message_actor.hs")).await
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
        directory.path().join("rows"),
        directory.path().join("cursor"),
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
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notification_barrier_never_enters_legacy_push_or_batch_ack() {
    let root = tempfile::tempdir().unwrap();
    let inbox =
        Arc::new(ActorInbox::open(root.path().join("rows"), root.path().join("cursor")).unwrap());
    let target = ActorRef::first(exomonad_actor::ActorId(7));
    inbox
        .publish(DurableActorEvent::Text("ordinary prefix".into()))
        .unwrap();
    inbox
        .publish_tracked(
            DurableActorEvent::Text("one-way text".into()),
            DeliveryProvenance::Notification {
                sender: ActorRef::first(exomonad_actor::ActorId(8)),
                target,
            },
        )
        .unwrap();
    inbox
        .publish(DurableActorEvent::Text("ordinary suffix".into()))
        .unwrap();
    // An old envelope decoder ignores receipt metadata but must accept every
    // payload before it gets the chance to reject the upgraded checkpoint.
    // A schema-invalid final row could otherwise trigger old tail repair.
    #[derive(Deserialize)]
    struct OldTextEnvelope {
        sequence: u64,
        payload: String,
    }
    let rows = std::fs::read_to_string(root.path().join("rows")).unwrap();
    let old_rows = rows
        .lines()
        .map(|line| serde_json::from_str::<OldTextEnvelope>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(old_rows.len(), 3);
    assert_eq!(old_rows[1].sequence, 2);
    assert_eq!(old_rows[1].payload, "one-way text");
    let backend = ScriptedPush {
        fail: std::sync::atomic::AtomicBool::new(false),
        messages: std::sync::Mutex::new(Vec::new()),
    };
    let binding = root.path().join("binding.json");
    exomonad_agent::accept_interactive_session_binding(
        &binding,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into()),
        None,
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding)
        .await
        .unwrap();
    let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
    let (producer, reconciliations) = test_delivery_dependencies(root.path(), target);
    deliver_pending(
        target,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .unwrap();
    // Unsupported normal steering remains a tracked barrier; it never
    // falls through to legacy push or acknowledges the suffix.
    assert!(deliver_pending(
        target,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .is_err());
    assert_eq!(*backend.messages.lock().unwrap(), vec!["ordinary prefix"]);
    assert_eq!(inbox.cursor(), 1);
    assert_eq!(inbox.watermark(), 3);
    assert!(matches!(
        inbox.pending(),
        Err(exomonad_node::InboxError::TrackedBarrier { sequence: 2 })
    ));
    assert!(inbox.legacy_pending_prefix().unwrap().is_empty());
    assert!(matches!(
        inbox.observe_receipt(2).unwrap(),
        exomonad_node::ReceiptLookup::Retained(evidence)
            if evidence.phase == exomonad_node::DeliveryPhase::Accepted
    ));
}

#[tokio::test]
async fn native_push_acknowledges_only_after_acceptance_and_retries_the_same_row() {
    let root = tempfile::tempdir().expect("inbox root");
    let inbox = Arc::new(
        ActorInbox::open(root.path().join("rows"), root.path().join("cursor")).expect("open inbox"),
    );
    inbox
        .publish(DurableActorEvent::Text("child completed".into()))
        .expect("publish");
    let backend = ScriptedPush {
        fail: std::sync::atomic::AtomicBool::new(true),
        messages: std::sync::Mutex::new(Vec::new()),
    };
    let binding_path = root.path().join("binding.json");
    exomonad_agent::accept_interactive_session_binding(
        &binding_path,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId("019fe92a-1a66-7820-9481-c0a2d108aba1".into()),
        None,
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding_path)
        .await
        .unwrap();
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
    let (producer, reconciliations) = test_delivery_dependencies(root.path(), actor);
    assert_eq!(
        orient_launch_instructions("event", &observation.snapshot()),
        "event"
    );
    observation.publish_launch_role(exomonad_actor::EffectiveRole::research(), 0);
    observation.publish_workspace(exomonad_actor::ActorWorkspaceObservation {
        workspace_path: "/tmp/visible".into(),
        host_storage_path: "/host/research".into(),
        worktree_id: Some("research-tree".into()),
        expected_branch: Some("research".into()),
    });
    let orientation = observation.snapshot().launch_orientation().unwrap();
    assert!(orientation.contains("role=Research"));
    assert!(orientation.contains("native_tools=InspectionOnly"));
    assert!(orientation.contains("workspace=InspectOnly"));
    assert!(orientation.contains("descendant_depth=0"));
    assert!(orientation.contains("workspace_path=\"/tmp/visible\" (native tools)"));
    assert!(orientation.contains("expected_branch=\"research\""));
    assert!(!orientation.contains("sessionReply"));
    assert_eq!(
        orient_launch_instructions("launch instructions", &observation.snapshot()),
        format!("launch instructions\n\n{orientation}")
    );

    assert!(deliver_pending(
        actor,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .is_err());
    assert_eq!(inbox.pending().expect("pending after refusal").len(), 1);
    assert_eq!(
        observation.snapshot().activation_kind,
        exomonad_actor::ActorActivationKind::RootStarted
    );
    inbox
        .publish(DurableActorEvent::Text("second event".into()))
        .expect("publish second event");

    backend
        .fail
        .store(false, std::sync::atomic::Ordering::SeqCst);
    deliver_pending(
        actor,
        &inbox,
        &thread,
        &backend,
        &producer,
        &reconciliations,
        root.path(),
        &observation,
    )
    .await
    .expect("retry accepted");
    assert!(inbox.pending().expect("acked inbox").is_empty());
    assert_eq!(
        *backend.messages.lock().unwrap(),
        [
            "child completed".to_owned(),
            "child completed\n\nsecond event".to_owned(),
        ]
    );
    assert_eq!(
        observation.snapshot().activation_kind,
        exomonad_actor::ActorActivationKind::EventsActivated {
            inbox_sequences: vec![1, 2]
        }
    );

    // First and retained requests use the same unwrapped message boundary.
    for sequence in [1, 2] {
        let message = format!("Request {sequence}: review\n\nReturn with `respond`.");
        inbox
            .publish(DurableActorEvent::Typed(TypedActorEvent::SessionReady {
                sequence,
                request: exomonad_actor::RequestId(sequence),
                input_type: "Text".into(),
                message: message.clone(),
            }))
            .unwrap();
        backend
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(deliver_pending(
            actor,
            &inbox,
            &thread,
            &backend,
            &producer,
            &reconciliations,
            root.path(),
            &observation,
        )
        .await
        .is_err());
        backend
            .fail
            .store(false, std::sync::atomic::Ordering::SeqCst);
        deliver_pending(
            actor,
            &inbox,
            &thread,
            &backend,
            &producer,
            &reconciliations,
            root.path(),
            &observation,
        )
        .await
        .unwrap();
        let messages = backend.messages.lock().unwrap();
        assert_eq!(&messages[messages.len() - 2..], &[message.clone(), message]);
        assert!(inbox.pending().unwrap().is_empty());
    }
}

#[tokio::test]
async fn retired_delivery_is_forced_without_claiming_tool_service_cleanup() {
    let actor = ActorRef::first(exomonad_actor::ActorId(7));
    let mut delivery = tokio::spawn(std::future::pending::<()>());
    let outcome = stop_retired_delivery(actor, &mut delivery, Duration::ZERO).await;
    assert!(delivery.is_finished());
    assert_eq!(outcome, CleanupComponentOutcome::Forced);

    let receipt = InteractiveCleanupReceipt {
        actor,
        components: vec![CleanupComponentReceipt {
            component: CleanupComponent::Delivery,
            outcome,
        }],
    };
    assert!(receipt.degraded());
    assert!(receipt
        .render()
        .contains("Delivery: forcibly stopped before graceful settlement"));
}

#[test]
fn panicked_retirement_preserves_every_cleanup_domain_as_unknown() {
    let actor = ActorRef::first(exomonad_actor::ActorId(8));
    let receipt = panicked_cleanup_receipt(actor);

    assert_eq!(receipt.actor, actor);
    assert_eq!(receipt.components.len(), 7);
    assert_eq!(
        receipt
            .components
            .iter()
            .map(|component| component.component)
            .collect::<Vec<_>>(),
        vec![
            CleanupComponent::Process,
            CleanupComponent::Pane,
            CleanupComponent::ToolService,
            CleanupComponent::Delivery,
            CleanupComponent::Socket,
            CleanupComponent::BuildResource,
            CleanupComponent::WorktreeBinding,
        ]
    );
    assert!(receipt.components.iter().all(|component| matches!(
        component.outcome,
        CleanupComponentOutcome::Failed { ref detail }
            if detail.contains("component completion is unknown")
    )));
    assert!(receipt.degraded());
}

#[test]
fn degraded_cleanup_receipt_preserves_each_component_without_becoming_fleet_failure() {
    let receipt = InteractiveCleanupReceipt {
        actor: ActorRef::first(exomonad_actor::ActorId(7)),
        components: vec![
            CleanupComponentReceipt {
                component: CleanupComponent::Process,
                outcome: CleanupComponentOutcome::Failed {
                    detail: "pane already unavailable".into(),
                },
            },
            CleanupComponentReceipt {
                component: CleanupComponent::Delivery,
                outcome: CleanupComponentOutcome::Completed,
            },
            CleanupComponentReceipt {
                component: CleanupComponent::WorktreeBinding,
                outcome: CleanupComponentOutcome::Failed {
                    detail: "binding journal unavailable".into(),
                },
            },
        ],
    };

    assert!(receipt.degraded());
    assert_eq!(receipt.components.len(), 3);
    let rendered = receipt.render();
    assert!(rendered.contains("Process: pane already unavailable"));
    assert!(rendered.contains("WorktreeBinding: binding journal unavailable"));
    assert!(rendered.contains("The host and sibling actors are unaffected."));
}

#[test]
fn worker_workspaces_are_distinct_linked_worktrees_in_one_git_namespace() {
    let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("README.md", "source\n", "seed")
        .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let (manager, _bindings) =
        actor_worktree_resources_at(storage.path(), repository.path()).unwrap();
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

#[ignore = "executeCleanup refuses on actor health (no confirmed idle provider turn) before it detects a stale plan; the test expects CleanupStalePlan first — decide the refusal order, then re-enable"]
#[tokio::test]
async fn typed_reply_settles_response_and_wakes_registered_watch() {
    fn fixture_items(source: &'static str) -> Vec<&'static str> {
        source
            .split("\n-- TIDEPOOL-ITEM --\n")
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .collect()
    }

    let mut campaign = test_campaign::TestCampaign::start().await;
    let mut deployments = campaign.take_deployments();
    let test_campaign::TestCampaign {
        _repository,
        _runtime,
        session_root,
        worktrees,
        bindings,
        authority,
        actor,
        hosted,
        root_installation,
        ..
    } = campaign;

    let inspected = dispatch_lookup(
        root_installation.policy.as_ref(),
        &["DefinitelyMissingFromExomonad", "request", "fmt"],
    )
    .await;
    assert_eq!(inspected["status"], "committed", "{inspected:?}");
    let inspection_text = inspected["items"][0]["output"].as_str().unwrap();
    assert!(inspection_text.contains("no match"), "{inspected:?}");
    assert!(inspection_text.contains("request"), "{inspected:?}");
    assert!(inspection_text.contains("fmt"), "{inspected:?}");
    let status = dispatch_status(root_installation.policy.as_ref(), "summary").await;
    assert_eq!(status["status"], "committed", "{status:?}");
    let ergonomics = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        include_str!("../actor_host_fixtures/generic_actor/workbench_ergonomics.hs"),
    )
    .await;
    assert_eq!(ergonomics["status"], "committed", "{ergonomics:?}");
    assert_eq!(ergonomics["items"][0]["status"], "committed");
    assert!(ergonomics["items"][0]["warnings"]
        .as_array()
        .is_some_and(|warnings| !warnings.is_empty()));
    assert_eq!(ergonomics["items"][1]["status"], "committed");
    assert_eq!(ergonomics["items"][2]["output"], "7");
    assert_eq!(ergonomics["items"][3]["output"], "value=7");

    // Execute the mounted documentation itself.
    let workbench_doc = include_str!("../../../../exomonad/prompts/docs/workbench.md");
    let (_, example) = workbench_doc.split_once("```haskell\n").unwrap();
    let (example, _) = example.split_once("```").unwrap();
    let documented = dispatch_haskell_script(root_installation.policy.as_ref(), example).await;
    assert_eq!(documented["status"], "committed", "{documented:?}");
    let items = documented["items"].as_array().unwrap();
    assert!(
        items.iter().all(|item| item["status"] == "committed"),
        "{documented:?}"
    );
    assert_eq!(items[items.len() - 1]["output"], "[7]", "{documented:?}");

    let setup_policy = Arc::clone(&root_installation.policy);
    let submitted = tokio::spawn(async move {
        dispatch_haskell(
            setup_policy.as_ref(),
            fixture_items(include_str!(
                "../actor_host_fixtures/generic_actor/reply_watch_roundtrip.hs"
            )),
        )
        .await
    });
    let child_installations = tokio::time::timeout(Duration::from_secs(180), async {
        let mut children = Vec::new();
        while children.len() < 3 {
            match deployments.recv().await {
                Some(LocalResidentDeployment::PolicyInstalled(installation))
                    if installation.actor.identity() != actor.identity() =>
                {
                    children.push(installation);
                }
                Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                    panic!("actor {actor:?} retired before child installation: {terminal:?}")
                }
                Some(_) => {}
                None => panic!("deployment channel closed before child installation"),
            }
        }
        children
    })
    .await
    .expect("child installation timeout");
    let worker_installation = child_installations
        .iter()
        .find(|installation| installation.label.ends_with("/worker"))
        .expect("worker installation")
        .clone();
    let witness_installation = child_installations
        .iter()
        .find(|installation| installation.label.ends_with("/witness"))
        .expect("witness installation")
        .clone();
    let scaffold_installation = child_installations
        .iter()
        .find(|installation| installation.label.ends_with("/scaffold"))
        .expect("scaffold installation")
        .clone();
    for installation in &child_installations {
        assert_eq!(installation.context_parent, Some(actor.identity()));
        assert_eq!(
            installation.fork_group,
            Some(exomonad_actor::ForkGroupId(1))
        );
        let expected_role = if installation.label.ends_with("/scaffold") {
            exomonad_actor::ActorRole::Coding
        } else {
            exomonad_actor::ActorRole::Research
        };
        assert_eq!(installation.effective_role.role(), expected_role);
        authority.install_grant(
            installation.actor.identity().into(),
            worktree_grant(expected_role),
        );
        let [worktree_id] = installation.launch_worktrees.as_slice() else {
            panic!("forked research actor did not receive one named worktree")
        };
        let worktree = worktrees
            .lookup(&exomonad_worktree::WorktreeId::from_raw(worktree_id))
            .expect("named worktree lookup")
            .expect("named worktree remains registered");
        assert_eq!(
            worktree.branch().as_str(),
            tidepool_repr::ActorPath::parse(&installation.label)
                .expect("allocated actor path")
                .git_branch()
        );
        let principal = WorktreePrincipal::exact_actor(
            &runtime_namespace(session_root.path()),
            installation.actor.identity().id.0,
            installation.actor.identity().incarnation.0,
        );
        assert!(installation.worktree_custody.is_some());
        assert_eq!(
            bindings.lock().current(worktree.id()).unwrap().agent(),
            &principal
        );
    }
    worker_installation
        .fork_gate
        .as_ref()
        .expect("context-fork child has an admission gate")
        .mark_ready()
        .expect("test host marks first child provider ready");
    assert!(
        !submitted.is_finished(),
        "one ready sibling must not publish a partially admitted unfold"
    );
    witness_installation
        .fork_gate
        .as_ref()
        .expect("context-fork sibling has an admission gate")
        .mark_ready()
        .expect("test host marks second child provider ready");
    scaffold_installation
        .fork_gate
        .as_ref()
        .expect("context-fork scaffold has an admission gate")
        .mark_ready()
        .expect("test host marks scaffold provider ready");
    let submitted = tokio::time::timeout(Duration::from_secs(120), submitted)
        .await
        .expect("request setup timed out")
        .expect("request setup task");
    assert_eq!(submitted["status"], "committed", "{submitted:?}");
    assert!(submitted["items"].as_array().is_some_and(|items| items
        .iter()
        .any(|item| item["operations"]
            .as_array()
            .is_some_and(|operations| !operations.is_empty()))));

    let pending_status = dispatch_status(root_installation.policy.as_ref(), "summary").await;
    let pending_status = pending_status["items"][0]["output"]
        .as_str()
        .expect("status output");
    assert!(
        pending_status.contains("application=attached"),
        "{pending_status}"
    );
    assert!(
        pending_status.contains("responses:")
            && pending_status.contains("\"worker\"")
            && pending_status.contains("\"witness\""),
        "{pending_status}"
    );
    assert!(
        pending_status.contains("watches:") && pending_status.contains("\"both-ready\""),
        "{pending_status}"
    );
    assert!(pending_status.contains("after=5min"), "{pending_status}");
    assert!(
        pending_status.contains("actors:\n")
            && pending_status.contains("role=Research")
            && pending_status.contains("role=Coding")
            && pending_status.contains("bound_worktree=Some("),
        "{pending_status}"
    );
    let lineage = dispatch_status(root_installation.policy.as_ref(), "lineage").await;
    let lineage = lineage["items"][0]["output"]
        .as_str()
        .expect("lineage output");
    for field in [
        "context_parent=",
        "haskell_scope=",
        "provider_thread=",
        "provider_parent_thread=",
        "cache_boundary=",
        "cached_input=",
        "uncached_input=",
    ] {
        assert!(lineage.contains(field), "missing {field}: {lineage}");
    }
    let launch_receipt = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "(responseAdmission (first3 workers), responseAdmission (second3 workers), responseAdmission (third3 workers))",
    )
    .await;
    let launch_receipt = launch_receipt["items"][0]["output"]
        .as_str()
        .expect("branch receipt output");
    assert!(
        launch_receipt.contains("ActorPath \"reply-watch/roundtrip/worker\"")
            && launch_receipt.contains("ActorPath \"reply-watch/roundtrip/witness\"")
            && launch_receipt.contains("ActorPath \"reply-watch/roundtrip/scaffold\"")
            && launch_receipt.matches("forkGroupIdentity = 1").count() == 3,
        "{launch_receipt}"
    );
    let scaffold_watch = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "scaffoldReadiness <- watch \"scaffold-ready\" (awaitSettled (third3 workers))",
    )
    .await;
    assert_eq!(scaffold_watch["status"], "committed", "{scaffold_watch:?}");

    let activations = tokio::time::timeout(Duration::from_secs(10), async {
        let mut activations = Vec::new();
        while activations.len() < 3 {
            match deployments.recv().await {
                Some(LocalResidentDeployment::SessionReady { activation })
                    if child_installations
                        .iter()
                        .any(|child| child.actor.identity() == activation.id.actor()) =>
                {
                    activations.push(activation);
                }
                Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                    panic!("actor {actor:?} retired before request activation: {terminal:?}")
                }
                Some(_) => {}
                None => panic!("deployment channel closed before request activation"),
            }
        }
        activations
    })
    .await
    .expect("request activation timeout");
    let worker_activation = activations
        .iter()
        .find(|activation| activation.id.actor() == worker_installation.actor.identity())
        .expect("worker activation");
    let witness_activation = activations
        .iter()
        .find(|activation| activation.id.actor() == witness_installation.actor.identity())
        .expect("witness activation");
    let scaffold_activation = activations
        .iter()
        .find(|activation| activation.id.actor() == scaffold_installation.actor.identity())
        .expect("scaffold activation");
    assert_eq!(worker_activation.input_type, "Int");
    assert_eq!(witness_activation.input_type, "Text");
    assert_eq!(scaffold_activation.input_type, "Text");

    let reply_bindings = dispatch_lookup(
        worker_installation.policy.as_ref(),
        &["sessionReply", "respond"],
    )
    .await;
    assert_eq!(reply_bindings["status"], "committed", "{reply_bindings:?}");
    let replied = dispatch_haskell_script(
        worker_installation.policy.as_ref(),
        "respond (ReplyReport (sessionInput + sharedDelta))",
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied:?}");
    assert_eq!(
        replied["items"][0]["terminalTransfer"], "replyAccepted",
        "{replied:?}"
    );
    assert!(replied["items"][0]["operations"]
        .as_array()
        .is_some_and(|operations| operations.iter().any(|operation| {
            operation["effect"] == "reply" && operation["disposition"] == "committed"
        })));
    let witnessed = dispatch_haskell_script(
        witness_installation.policy.as_ref(),
        "respond (EchoReport sessionInput)",
    )
    .await;
    assert_eq!(witnessed["status"], "replied", "{witnessed:?}");

    let notification = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::WatchChanged { notification })
                    if notification.owner == actor.identity() =>
                {
                    break notification;
                }
                Some(_) => {}
                None => panic!("deployment channel closed before watch transition"),
            }
        }
    })
    .await
    .expect("watch transition timeout");
    assert_eq!(
        notification.transition,
        exomonad_actor::WatchTransition::Ready
    );

    let observed = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "inspectFull <$> pollResponse (first3 workers)\ninspectFull <$> pollResponse (second3 workers)\ninspectFull <$> pollWatch readiness",
    )
    .await;
    assert_eq!(observed["status"], "committed", "{observed:?}");
    assert!(
        observed["items"][0]["output"]
            .as_str()
            .is_some_and(|output| {
                output.contains("ResponseReady")
                    && output.contains("responseValue = ReplyReport 42")
                    && output.contains("responseWorktree = WorktreeObserved")
            }),
        "{observed:?}"
    );
    assert!(
        observed["items"][1]["output"]
            .as_str()
            .is_some_and(|output| {
                output.contains("ResponseReady")
                    && output.contains("responseValue = EchoReport \"cache\"")
            }),
        "{observed:?}"
    );
    assert!(
        observed["items"][2]["output"]
            .as_str()
            .is_some_and(|output| {
                output.contains("WatchReady")
                    && output.contains("ReplyReport 42")
                    && output.contains("EchoReport \"cache\"")
            }),
        "{observed:?}"
    );

    let group_observation = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "maybe (pure Nothing) (fmap (fmap (length . groupRoster)) . observeForkGroup) (forkGroupHandle (first3 workers))",
    )
    .await;
    assert_eq!(
        group_observation["status"], "committed",
        "{group_observation:?}"
    );
    assert_eq!(group_observation["items"][0]["output"], "Just 3");

    let ready_status = dispatch_status(root_installation.policy.as_ref(), "summary").await;
    let ready_status = ready_status["items"][0]["output"]
        .as_str()
        .expect("status output");
    assert!(ready_status.contains("\"scaffold\""), "{ready_status}");
    assert!(ready_status.contains("\"worker\""), "{ready_status}");
    assert!(ready_status.contains("\"witness\""), "{ready_status}");
    assert!(ready_status.contains("\"both-ready\""), "{ready_status}");

    let cleanup_preview = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "let Just staleGroup = forkGroupHandle (first3 workers)\nstaleCleanup <- planCleanup staleGroup",
    )
    .await;
    assert_eq!(
        cleanup_preview["status"], "committed",
        "{cleanup_preview:?}"
    );

    let followup = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        include_str!("../actor_host_fixtures/generic_actor/late_refinement.hs"),
    )
    .await;
    assert_eq!(followup["status"], "committed", "{followup:?}");
    let refinement_doc = include_str!("../../../../exomonad/prompts/docs/refinement.md");
    let (_, example) = refinement_doc.split_once("```haskell\n").unwrap();
    let (example, _) = example.split_once("```").unwrap();
    let refinement = dispatch_haskell_script(root_installation.policy.as_ref(), example).await;
    assert_eq!(refinement["status"], "committed", "{refinement:?}");
    let followup_binding =
        dispatch_haskell_script(root_installation.policy.as_ref(), "let followup = revision").await;
    assert_eq!(
        followup_binding["status"], "committed",
        "{followup_binding:?}"
    );
    let followup_watch = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "followupReadiness <- watch (case watchLabel \"revision-ready\" of { Right value -> value; Left _ -> error \"fixture watch\" }) (awaitResponse followup)",
    )
    .await;
    assert_eq!(followup_watch["status"], "committed", "{followup_watch:?}");
    let followup_activation = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::SessionReady { activation })
                    if activation.id.actor() == worker_installation.actor.identity() =>
                {
                    break activation;
                }
                Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                    panic!("actor {actor:?} retired before follow-up activation: {terminal:?}")
                }
                Some(_) => {}
                None => panic!("deployment channel closed before follow-up activation"),
            }
        }
    })
    .await
    .expect("follow-up activation timeout");
    assert!(followup_activation.input_type.ends_with("LateRefinement"));
    let followup_reply = dispatch_haskell_script(
        worker_installation.policy.as_ref(),
        "respond (LateReport (refinementTransform sessionInput (refinementValue sessionInput) + sharedDelta))",
    )
    .await;
    assert_eq!(followup_reply["status"], "replied", "{followup_reply:?}");
    let followup_notification = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::WatchChanged { notification })
                    if notification.owner == actor.identity() =>
                {
                    break notification;
                }
                Some(_) => {}
                None => panic!("deployment channel closed before follow-up watch transition"),
            }
        }
    })
    .await
    .expect("follow-up watch transition timeout");
    assert_eq!(
        followup_notification.transition,
        exomonad_actor::WatchTransition::Ready
    );
    let followup_result = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "inspectFull <$> pollResponse followup",
    )
    .await;
    assert_eq!(
        followup_result["status"], "committed",
        "{followup_result:?}"
    );
    assert!(
        followup_result["items"][0]["output"]
            .as_str()
            .is_some_and(|output| output.contains("responseValue = LateReport 101")),
        "{followup_result:?}"
    );

    let stale_cleanup = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "executeCleanup staleCleanup",
    )
    .await;
    assert_eq!(stale_cleanup["status"], "committed", "{stale_cleanup:?}");
    let refusal = stale_cleanup["items"][0]["output"].as_str().unwrap();
    assert!(refusal.contains("CleanupStalePlan"), "{refusal}");
    assert!(!refusal.contains("CleanupStoppedActor"), "{refusal}");
    assert!(
        refusal.contains("cleanupReceiptComplete = False"),
        "{refusal}"
    );

    let scaffold_policy = Arc::clone(&scaffold_installation.policy);
    let nested_submitted = tokio::spawn(async move {
        dispatch_haskell_script(
            scaffold_policy.as_ref(),
            "nested <- unfold (subgroup \"leaves\") ((,) <$> child (coding @ReplyReport currentCheckout (assignment [label|implementation|] (7 :: Int))) <*> child (coding @EchoReport currentCheckout (assignment [label|verification|] (\"nested\" :: Text))))",
        )
        .await
    });
    let nested_installations = tokio::time::timeout(Duration::from_secs(60), async {
        let mut children = Vec::new();
        while children.len() < 2 {
            match deployments.recv().await {
                Some(LocalResidentDeployment::PolicyInstalled(installation))
                    if installation.context_parent
                        == Some(scaffold_installation.actor.identity()) =>
                {
                    children.push(installation);
                }
                Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                    panic!("nested actor {actor:?} retired during admission: {terminal:?}")
                }
                Some(_) => {}
                None => panic!("deployment channel closed during nested admission"),
            }
        }
        children
    })
    .await
    .expect("nested child installation timeout");
    let implementation = nested_installations
        .iter()
        .find(|installation| installation.label.ends_with("/implementation"))
        .expect("nested implementation installation")
        .clone();
    let verification = nested_installations
        .iter()
        .find(|installation| installation.label.ends_with("/verification"))
        .expect("nested verification installation")
        .clone();
    let scaffold_tree_id =
        exomonad_worktree::WorktreeId::from_raw(scaffold_installation.launch_worktrees[0].clone());
    let scaffold_tree = worktrees
        .lookup(&scaffold_tree_id)
        .expect("scaffold lookup")
        .expect("scaffold worktree retained");
    let scaffold_head = worktrees
        .git()
        .try_run(scaffold_tree.cwd(), &["rev-parse", "HEAD"])
        .expect("scaffold head")
        .trimmed()
        .to_string();
    for installation in &nested_installations {
        assert_eq!(
            installation.effective_role.role(),
            exomonad_actor::ActorRole::Coding
        );
        assert_eq!(
            installation.effective_role.descendants().maximum_depth + 1,
            scaffold_installation
                .effective_role
                .descendants()
                .maximum_depth
        );
        assert!(installation
            .effective_role
            .effect_keys()
            .contains(&exomonad_actor::ActorEffectKey::Forks));
        let worktree_id =
            exomonad_worktree::WorktreeId::from_raw(installation.launch_worktrees[0].clone());
        let worktree = worktrees
            .lookup(&worktree_id)
            .expect("nested worktree lookup")
            .expect("nested worktree retained");
        assert_eq!(worktree.source_head().as_str(), scaffold_head);
        assert_eq!(
            worktree.branch().as_str(),
            tidepool_repr::ActorPath::parse(&installation.label)
                .expect("allocated nested actor path")
                .git_branch()
        );
        let principal = WorktreePrincipal::exact_actor(
            &runtime_namespace(session_root.path()),
            installation.actor.identity().id.0,
            installation.actor.identity().incarnation.0,
        );
        assert!(installation.worktree_custody.is_some());
        assert_eq!(
            bindings.lock().current(worktree.id()).unwrap().agent(),
            &principal
        );
        installation
            .fork_gate
            .as_ref()
            .expect("nested fork gate")
            .mark_ready()
            .expect("mark nested provider ready");
    }
    let nested_submitted = tokio::time::timeout(Duration::from_secs(120), nested_submitted)
        .await
        .expect("nested unfold timed out")
        .expect("nested unfold task");
    assert_eq!(
        nested_submitted["status"], "committed",
        "{nested_submitted:?}"
    );
    let nested_watch = dispatch_haskell_script(
        scaffold_installation.policy.as_ref(),
        "nestedReady <- watch \"leaves-ready\" ((,) <$> awaitResponse (fst nested) <*> awaitResponse (snd nested))",
    )
    .await;
    assert_eq!(nested_watch["status"], "committed", "{nested_watch:?}");

    let nested_activations = tokio::time::timeout(Duration::from_secs(10), async {
        let mut activations = Vec::new();
        while activations.len() < 2 {
            match deployments.recv().await {
                Some(LocalResidentDeployment::SessionReady { activation })
                    if nested_installations
                        .iter()
                        .any(|child| child.actor.identity() == activation.id.actor()) =>
                {
                    activations.push(activation);
                }
                Some(LocalResidentDeployment::Retired { actor, terminal }) => {
                    panic!("nested actor {actor:?} retired before activation: {terminal:?}")
                }
                Some(_) => {}
                None => panic!("deployment channel closed before nested activation"),
            }
        }
        activations
    })
    .await
    .expect("nested activation timeout");
    assert!(nested_activations.iter().any(|activation| {
        activation.id.actor() == implementation.actor.identity() && activation.input_type == "Int"
    }));
    assert!(nested_activations.iter().any(|activation| {
        activation.id.actor() == verification.actor.identity() && activation.input_type == "Text"
    }));

    for (installation, path, contents) in [
        (&implementation, "implementation.txt", "implemented\n"),
        (&verification, "verification.txt", "verified\n"),
    ] {
        let tree = worktrees
            .lookup(&exomonad_worktree::WorktreeId::from_raw(
                installation.launch_worktrees[0].clone(),
            ))
            .expect("lookup nested commit tree")
            .expect("nested commit tree retained");
        std::fs::write(tree.cwd().join(path), contents).expect("write nested candidate");
        worktrees
            .git()
            .try_run(tree.cwd(), &["add", path])
            .expect("stage nested candidate");
        worktrees
            .git()
            .try_run(tree.cwd(), &["commit", "-m", path])
            .expect("commit nested candidate");
    }
    let implementation_reply = dispatch_haskell_script(
        implementation.policy.as_ref(),
        "respond (ReplyReport (sessionInput + sharedDelta))",
    )
    .await;
    assert_eq!(
        implementation_reply["status"], "replied",
        "{implementation_reply:?}"
    );
    let peer_setup = dispatch_haskell_script(
        verification.policy.as_ref(),
        include_str!("../actor_host_fixtures/generic_actor/peer_revision.hs"),
    )
    .await;
    assert_eq!(peer_setup["status"], "committed", "{peer_setup:?}");
    let peer_repair = dispatch_haskell_script(verification.policy.as_ref(), example).await;
    assert_eq!(peer_repair["status"], "committed", "{peer_repair:?}");
    let repair_watch_example = refinement_doc
        .split("```haskell\n")
        .nth(2)
        .unwrap()
        .split_once("```")
        .unwrap()
        .0;
    let peer_watch =
        dispatch_haskell_script(verification.policy.as_ref(), repair_watch_example).await;
    assert_eq!(peer_watch["status"], "committed", "{peer_watch:?}");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::SessionReady { activation })
                    if activation.id.actor() == implementation.actor.identity() =>
                {
                    break
                }
                Some(_) => {}
                None => panic!("deployment channel closed before peer repair"),
            }
        }
    })
    .await
    .expect("peer repair activation timeout");
    let repaired = dispatch_haskell_script(
        implementation.policy.as_ref(),
        "respond (ReplyReport (sessionInput + sharedDelta))",
    )
    .await;
    assert_eq!(repaired["status"], "replied", "{repaired:?}");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::WatchChanged { notification })
                    if notification.owner == verification.actor.identity()
                        && notification.transition == exomonad_actor::WatchTransition::Ready =>
                {
                    break
                }
                Some(_) => {}
                None => panic!("deployment channel closed before peer review wake"),
            }
        }
    })
    .await
    .expect("peer review wake timeout");
    let peer_result = dispatch_haskell_script(
        verification.policy.as_ref(),
        "inspectFull <$> pollWatch repairReady",
    )
    .await;
    assert_eq!(peer_result["status"], "committed", "{peer_result:?}");
    assert!(
        peer_result["items"][0]["output"]
            .as_str()
            .is_some_and(|output| output.contains("ReplyReport 11")),
        "{peer_result:?}"
    );
    let verification_reply = dispatch_haskell_script(
        verification.policy.as_ref(),
        "respond (EchoReport sessionInput)",
    )
    .await;
    assert_eq!(
        verification_reply["status"], "replied",
        "{verification_reply:?}"
    );
    let nested_notification = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::WatchChanged { notification })
                    if notification.owner == scaffold_installation.actor.identity() =>
                {
                    break notification;
                }
                Some(_) => {}
                None => panic!("deployment channel closed before nested watch wake"),
            }
        }
    })
    .await
    .expect("nested watch wake timeout");
    assert_eq!(
        nested_notification.transition,
        exomonad_actor::WatchTransition::Ready
    );

    let folded = dispatch_haskell_script(
        scaffold_installation.policy.as_ref(),
        include_str!("nested_merge_fold.hs"),
    )
    .await;
    assert_eq!(folded["status"], "replied", "{folded:?}");
    assert!(scaffold_tree.cwd().join("implementation.txt").is_file());
    assert!(scaffold_tree.cwd().join("verification.txt").is_file());

    let scaffold_notification = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match deployments.recv().await {
                Some(LocalResidentDeployment::WatchChanged { notification })
                    if notification.owner == actor.identity() =>
                {
                    break notification
                }
                Some(_) => {}
                None => panic!("deployment channel closed before scaffold watch wake"),
            }
        }
    })
    .await
    .expect("scaffold watch wake timeout");
    assert_eq!(
        scaffold_notification.transition,
        exomonad_actor::WatchTransition::Ready
    );
    let scaffold_result = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "inspectFull <$> pollWatch scaffoldReadiness",
    )
    .await;
    assert!(scaffold_result["items"][0]["output"]
        .as_str()
        .is_some_and(|output| {
            output.contains("ReplyAvailable") && output.contains("ScaffoldReport \"folded\"")
        }));

    for installation in child_installations
        .iter()
        .chain(nested_installations.iter())
    {
        installation
            .runtime_observation
            .publish_provider_observation(exomonad_agent::ProviderObservation {
                turn: Some(exomonad_agent::ProviderTurnObservation {
                    thread: format!("test-{}", installation.actor.identity().id.0),
                    turn: "completed".into(),
                    revision: 1,
                    state: exomonad_agent::ProviderTurnState::Succeeded,
                }),
                ..Default::default()
            });
    }
    let cleanup_doc = include_str!("../../../../exomonad/prompts/docs/cleanup.md");
    let cleanup_examples = cleanup_doc
        .split("```haskell\n")
        .skip(1)
        .map(|section| section.split_once("```").unwrap().0)
        .collect::<Vec<_>>();
    let cleanup_plan = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        &format!("let oneWorker = first3 workers\n{}", cleanup_examples[0]),
    )
    .await;
    assert_eq!(cleanup_plan["status"], "committed", "{cleanup_plan:?}");
    assert!(
        cleanup_plan["items"][2]["output"]
            .as_str()
            .is_some_and(|output| output.contains("cleanupPlanRefusal = Nothing")),
        "{cleanup_plan:?}"
    );

    let stale_runtime = &child_installations[0].runtime_observation;
    let confirmed_turn = stale_runtime.snapshot().provider_turn;
    stale_runtime.mark_provider_observation_stale();
    let blocked = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "executeCleanup cleanupPlan",
    )
    .await;
    assert_eq!(blocked["status"], "committed", "{blocked:?}");
    let blocked_output = blocked["items"][0]["output"].as_str().unwrap();
    assert!(blocked_output.contains("CleanupBlocked"), "{blocked:?}");
    assert!(!blocked_output.contains("CleanupForgot"), "{blocked:?}");
    stale_runtime.publish_provider_observation(exomonad_agent::ProviderObservation {
        turn: confirmed_turn,
        ..Default::default()
    });

    let cleanup = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        &format!("{}\ncleanupReceipt", cleanup_examples[1]),
    )
    .await;
    assert_eq!(cleanup["status"], "committed", "{cleanup:?}");
    assert!(
        cleanup["items"][1]["output"]
            .as_str()
            .is_some_and(|output| {
                output.contains("cleanupReceiptComplete = True")
                    && output.contains("CleanupGroupRetired")
            }),
        "{cleanup:?}"
    );

    let cleanup_retry = dispatch_haskell_script(
        root_installation.policy.as_ref(),
        "executeCleanup cleanupPlan",
    )
    .await;
    assert_eq!(cleanup_retry["status"], "committed", "{cleanup_retry:?}");
    assert!(cleanup_retry["items"][0]["output"]
        .as_str()
        .is_some_and(|output| output.contains("cleanupReceiptComplete = True")));

    actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "test complete".into(),
        })
        .await
        .expect("shutdown root");
    hosted.await.expect("root actor task");
}
