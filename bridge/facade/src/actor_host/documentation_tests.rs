//! Focused execution of the published examples through the real resident tool.

use super::test_campaign::TestCampaign;
use super::test_campaign::{
    dispatch_haskell_script, dispatch_haskell_script_result, dispatch_lookup, dispatch_status,
    original_tool_call,
};
use super::*;
use exomonad_tool::{
    ConversationOrigin, OriginalOperation, ToolArguments, ToolInvocation, ToolInvocationContext,
};

fn example(document: &str) -> &str {
    examples(document).next().unwrap()
}

fn examples(document: &str) -> impl Iterator<Item = &str> {
    document
        .split("```haskell\n")
        .skip(1)
        .map(|block| block.split_once("```").unwrap().0)
}

#[tokio::test]
async fn deferred_inherited_child_preserves_original_owner_after_parent_shadowing() {
    assert_deferred_original_owner(
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host_fixtures/generic_actor/deferred_original_owner.hs",
        ),
        false,
    )
    .await;
}

#[tokio::test]
async fn deferred_checkpoint_child_preserves_prior_nominal_owner_after_publication() {
    assert_deferred_original_owner(
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host_fixtures/generic_actor/deferred_checkpoint_original_owner.hs"),
        true,
    )
    .await;
}

async fn assert_deferred_original_owner(source: &str, checkpoint: bool) {
    let mut campaign = TestCampaign::start().await;
    let root = Arc::clone(&campaign.root_installation.policy);
    let call_id = format!("deferred-original-owner-{checkpoint}");
    let (invocation, completion) = original_tool_call(OriginalOperation {
        origin: ConversationOrigin::External {
            thread_id: "original-owner-test".into(),
        },
        request_id: call_id.clone(),
        call_id,
    });
    committed(root.as_ref(), "data DeferredInput = DeferredInput Int deriving Show\ndata DeferredReply = DeferredReply Int deriving Show").await;
    let result = root
        .dispatch_boxed(ToolInvocation {
            context: Some(invocation),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(source.into()),
        })
        .await
        .unwrap()
        .into_json()
        .expect("structured observer receives the typed workbench response");
    assert_eq!(result["status"], "committed", "{result}");
    campaign.assert_no_deployment("deferred child booted before completion", |event| {
        matches!(
            event,
            LocalResidentDeployment::PolicyInstalled(_)
                | LocalResidentDeployment::SessionReady { .. }
        )
    });
    committed(root.as_ref(), "data DeferredInput = LaterInput Bool deriving Show\ndata DeferredReply = LaterReply Bool deriving Show").await;
    let staged_children: Vec<_> = campaign
        .forest
        .inspect_host_graph()
        .into_iter()
        .filter(|node| node.creator == Some(campaign.actor.identity()))
        .collect();
    let [staged_child] = staged_children.as_slice() else {
        panic!("deferred call must retain exactly one staged child: {staged_children:?}");
    };
    // Installation is recorded before its deployment event, so this checks
    // the lifecycle owner rather than only a buffered observer channel.
    assert!(!staged_child.model_actor, "{staged_child:?}");
    assert!(staged_child.terminal.is_none(), "{staged_child:?}");
    let staged_identity = staged_child.actor;
    root.complete_boxed(completion.clone()).await.unwrap();
    let child = campaign
        .next_deployment(
            "original child tool installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                LocalResidentDeployment::Retired { actor, terminal } => {
                    panic!("captured child {actor:?} retired before installation: {terminal:?}")
                }
                other => Err(other),
            },
        )
        .await;
    assert_eq!(child.checkpoint.is_some(), checkpoint);
    let child_id = child.actor.identity();
    assert_eq!(child_id, staged_identity);
    campaign
        .next_deployment(
            "original typed child input",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == child_id =>
                {
                    Ok(activation)
                }
                LocalResidentDeployment::Retired { actor, terminal } => {
                    panic!("captured child {actor:?} retired before activation: {terminal:?}")
                }
                other => Err(other),
            },
        )
        .await;
    if checkpoint {
        committed(
            root.as_ref(),
            "Right () <- releaseCheckpoint originalOwnerSeed\npure ()",
        )
        .await;
    }
    let parent_failure = dispatch_haskell_script_result(
        root.as_ref(),
        "error \"parent-failure-after-deferred-release\" >> pure ()",
    )
    .await
    .expect_err("the parent cell deliberately fails after deferred publication");
    assert_authored_workbench_failure(
        parent_failure,
        campaign.actor.identity(),
        "parent-failure-after-deferred-release",
    );
    assert!(campaign.actor.terminal().get().is_none());
    let reply = dispatch_haskell_script(
        child.policy.as_ref(),
        "case sessionInput of DeferredInput n -> respond (DeferredReply (n + 1))",
    )
    .await;
    assert_eq!(reply["status"], "replied", "{reply}");
    let observed = displayed(
        &mut campaign,
        root.as_ref(),
        "originalOwnerReply <- pollResponse originalOwnerWorker\ndisplay (show originalOwnerReply)",
    )
    .await;
    let text = explicit_display_output(&observed)["text"].as_str().unwrap();
    assert!(text.contains("DeferredReply 42"), "{observed}");
    assert!(!text.contains("LaterReply"), "{observed}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn failed_deferred_capture_does_not_release_child_tools() {
    let mut campaign = TestCampaign::start().await;
    let root = Arc::clone(&campaign.root_installation.policy);
    let call_id = "deferred-original-owner-failure".to_owned();
    let (invocation, completion) = original_tool_call(OriginalOperation {
        origin: ConversationOrigin::External {
            thread_id: "original-owner-test".into(),
        },
        request_id: call_id.clone(),
        call_id,
    });
    committed(root.as_ref(), "data DeferredInput = DeferredInput Int deriving Show\ndata DeferredReply = DeferredReply Int deriving Show").await;
    let source = format!(
        "{}\nerror \"deferred-before-commit-failure\" >> pure ()",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host_fixtures/generic_actor/deferred_original_owner.hs"
        )
    );
    let result = root
        .dispatch_boxed(ToolInvocation {
            context: Some(invocation),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(source),
        })
        .await
        .expect_err("the deferred group creator deliberately fails before completion");
    assert_authored_workbench_failure(
        result,
        campaign.actor.identity(),
        "deferred-before-commit-failure",
    );
    assert!(campaign.actor.terminal().get().is_none());
    root.complete_boxed(completion.clone()).await.unwrap();
    root.complete_boxed(completion).await.unwrap();
    let aborted_children: Vec<_> = campaign
        .forest
        .inspect_host_graph()
        .into_iter()
        .filter(|node| node.creator == Some(campaign.actor.identity()))
        .collect();
    let [aborted_child] = aborted_children.as_slice() else {
        panic!("failed deferred call must retain its staged child: {aborted_children:?}");
    };
    // Failed completion awaits group abort and child shutdown. Retirement
    // keeps the installation flag, making a premature launch observable.
    assert!(!aborted_child.model_actor, "{aborted_child:?}");
    assert_eq!(
        aborted_child
            .terminal
            .as_ref()
            .map(|terminal| terminal.kind),
        Some(exomonad_actor::ActorExitKind::Cancelled),
        "{aborted_child:?}"
    );
    campaign.assert_no_deployment("failed deferred group released a child", |event| {
        matches!(
            event,
            LocalResidentDeployment::PolicyInstalled(_)
                | LocalResidentDeployment::SessionReady { .. }
        )
    });
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

fn assert_authored_workbench_failure(
    error: exomonad_actor::ResidentToolError,
    actor: exomonad_actor::ActorRef,
    marker: &str,
) {
    let exomonad_actor::ResidentToolError::Invocation(
        exomonad_actor::KernelInvocationFailure::Workbench(failure),
    ) = error
    else {
        panic!("expected an authored workbench failure: {error:?}");
    };
    assert_eq!(failure.actor, actor);
    assert!(matches!(
        failure.point,
        tidepool_runtime::session::WorkbenchFailurePoint::InputUnit { .. }
    ));
    assert!(failure.detail.contains(marker), "{failure:?}");
}

async fn committed(
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    let result = dispatch_haskell_script(policy, source).await;
    assert_eq!(result["status"], "committed", "{result:?}");
    result
}

async fn displayed(
    campaign: &mut TestCampaign,
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    campaign
        .drive_actor_output(&store, committed(policy, source))
        .await
}

#[tokio::test]
async fn colon_commands_and_ghci_groups_are_rejected_as_haskell_cells() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();
    for source in [":status", ":{\ncolonOnly = 1\n:}"] {
        let result = dispatch_haskell_script(policy, source).await;
        assert_eq!(result["status"], "rejected", "source={source}: {result}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn explicit_display_preserves_resource_guidance_and_specialized_renderers() {
    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    for (source, expected) in [
        (
            &tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/notebook_actor_scoped_handle.hs",
            ),
            "resource control guidance rendered",
        ),
        (
            &tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/notebook_display_generic.hs",
            ),
            "NotebookPlain {1 = 3, 2 = <function>}",
        ),
        (
            &tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/notebook_display_custom.hs",
            ),
            "custom-display-wins",
        ),
    ] {
        let reply = campaign
            .drive_actor_output(&store, committed(policy.as_ref(), source))
            .await;
        let text = explicit_display_output(&reply)["text"].as_str().unwrap();
        assert_eq!(text, expected, "{reply}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn explicit_display_is_the_only_value_presentation() {
    use super::command_test_support::{backend_request, TestCommands};

    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let executing_policy = policy.clone();
    let mut running = tokio::spawn(async move {
        committed(
            executing_policy.as_ref(),
            &tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/notebook_retained_expression_history.hs",
            ),
        )
        .await
    });
    let backend = TestCommands::completed("abc");
    tokio::select! {
        request = backend_request(&mut campaign) => request.supply(Ok(backend.clone())),
        result = &mut running => panic!("expression history ended before its command effect: {result:?}"),
    }
    let bare = running.await.unwrap();
    let items = bare["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{bare}");
    let captures = items
        .iter()
        .map(|item| {
            assert_eq!(item["kind"], "expression", "{bare}");
            assert_eq!(item["status"], "committed", "{bare}");
            let installed = item["installedBindings"].as_array().unwrap();
            assert_eq!(installed.len(), 1, "one capture per expression: {bare}");
            installed[0].as_str().unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        captures
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "each expression owns its retained capture: {bare}"
    );
    assert!(
        items.iter().all(|item| {
            item["operations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|operation| operation.get("display").is_none())
        }),
        "bare and effectful expressions retain values without display: {bare}"
    );
    campaign.assert_no_deployment("bare expression must not publish a display", |event| {
        matches!(event, LocalResidentDeployment::DisplayPublished(_))
    });
    let origin = harness::store::actor_output::ActorOutputOrigin {
        run: super::runtime_namespace(campaign.session_root.path()),
        native_actor: campaign.actor.identity().id.0,
        incarnation: campaign.actor.identity().incarnation.0,
    };
    assert!(store
        .actor_output_page(&origin, 0, 10)
        .unwrap()
        .outputs
        .is_empty());
    assert_eq!(backend.executions(), 1);

    let retained = committed(
        policy.as_ref(),
        &format!(
            "retainedHistory <- pure ({} (), {} (), {} ())",
            captures[0], captures[1], captures[2]
        ),
    )
    .await;
    assert_eq!(
        retained["items"][0]["installedBindings"],
        serde_json::json!(["retainedHistory"])
    );
    campaign.assert_no_deployment("reusing captures must not publish a display", |event| {
        matches!(event, LocalResidentDeployment::DisplayPublished(_))
    });
    let shown = campaign
        .drive_actor_output(
            &store,
            committed(
                policy.as_ref(),
                "display (let (a, b, text) = retainedHistory in if a == 41 && b == 42 && text == \"abc\" then a + b + T.length text else (-1 :: Int))",
            ),
        )
        .await;
    assert_eq!(explicit_display_output(&shown)["text"], "86", "{shown}");
    for (source, expected) in [
        ("display (inspectFull True)", "True"),
        (
            "display (inspectFull (\"first\\nsecond\" :: Text))",
            "first\nsecond",
        ),
    ] {
        let inspected = campaign
            .drive_actor_output(&store, committed(policy.as_ref(), source))
            .await;
        assert_eq!(
            explicit_display_output(&inspected)["text"],
            expected,
            "{inspected}"
        );
    }
    assert_eq!(
        backend.executions(),
        1,
        "later capture use and explicit display must not replay the command"
    );
    assert_eq!(
        store
            .actor_output_page(&origin, 0, 10)
            .unwrap()
            .outputs
            .len(),
        3
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_relocates_same_cell_types_and_rejects_before_installation() {
    let mut campaign = TestCampaign::start().await;
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let root = campaign.root_installation.policy.clone();

    let setup = campaign
        .drive_actor_output(
            &store,
            committed(
                root.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/notebook_nominal_setup.hs",
                ),
            ),
        )
        .await;
    assert_eq!(
        setup["summary"], "3 declarations, 2 statements, 1 expression",
        "{setup:?}"
    );
    let items = setup["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "{setup:?}");
    for (item, kind, start_line) in [
        (&items[0], "declaration", 1),
        (&items[1], "statement", 7),
        (&items[2], "statement", 9),
        (&items[3], "expression", 11),
    ] {
        assert_eq!(item["kind"], kind, "{setup:?}");
        assert_eq!(item["span"]["startLine"], start_line, "{setup:?}");
        assert_eq!(item["span"]["startColumn"], 1, "{setup:?}");
    }
    let declaration_sources = items[0]["sourceItems"].as_array().unwrap();
    assert_eq!(declaration_sources.len(), 3, "{setup:?}");
    assert_eq!(
        declaration_sources
            .iter()
            .map(|item| item["ordinal"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "{setup:?}"
    );
    assert_eq!(
        declaration_sources
            .iter()
            .map(|item| item["span"]["startLine"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 4],
        "{setup:?}"
    );
    assert!(
        declaration_sources
            .iter()
            .all(|item| item["kind"] == "declaration"),
        "{setup:?}"
    );
    assert_eq!(items[1]["sourceItems"][0]["ordinal"], 3, "{setup:?}");
    assert_eq!(items[3]["status"], "committed", "{setup:?}");
    assert_eq!(
        explicit_display_output(&setup)["text"],
        "Nothing",
        "{setup}"
    );

    let rejected = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/notebook_nominal_rejected.hs",
        ),
    )
    .await;
    assert_eq!(rejected["status"], "rejected", "{rejected:?}");
    let rejection = &rejected["items"][0];
    assert_eq!(rejection["failureLayer"], "compile", "{rejected:?}");
    assert!(
        rejection["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| {
                diagnostic["location"]["startLine"] == 6
                    && diagnostic["message"].as_str().is_some_and(|message| {
                        message.contains("IsString Int") && message.contains("bad")
                    })
            }),
        "{rejected:?}"
    );

    let missing_declaration = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/notebook_nominal_scope_probe.hs",
        ),
    )
    .await;
    assert_eq!(
        missing_declaration["status"], "rejected",
        "{missing_declaration:?}"
    );
    assert!(
        missing_declaration.to_string().contains("MustNotCommit")
            && missing_declaration.to_string().contains("not in scope"),
        "{missing_declaration:?}"
    );

    let missing_binding = dispatch_haskell_script(root.as_ref(), "willNotRun").await;
    assert_eq!(missing_binding["status"], "rejected", "{missing_binding:?}");
    assert!(
        missing_binding.to_string().contains("willNotRun")
            && missing_binding.to_string().contains("not in scope"),
        "{missing_binding:?}"
    );

    let rejected = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/notebook_prefix_compile_failure.hs",
        ),
    )
    .await;
    assert_eq!(rejected["status"], "rejected", "{rejected}");
    assert_eq!(
        rejected["items"][0]["failureLayer"], "compile",
        "{rejected}"
    );
    assert!(
        rejected.to_string().contains("prefixIdentifierMissing"),
        "{rejected}"
    );
    for binding in ["actorsBeforeFailure", "prefixValue", "tailValue"] {
        let missing = dispatch_haskell_script(root.as_ref(), binding).await;
        assert_eq!(missing["status"], "rejected", "{missing}");
        assert!(missing.to_string().contains("not in scope"), "{missing}");
    }

    committed(root.as_ref(), "shadowed <- pure (1 :: Int)\n").await;
    let shadowed = campaign
        .drive_actor_output(
            &store,
            committed(
                root.as_ref(),
                "shadowed <- pure (2 :: Int)\n_ <- display shadowed\n",
            ),
        )
        .await;
    assert_eq!(
        explicit_display_output(&shadowed)["text"],
        "2",
        "{shadowed}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_prologue_applies_to_check_stage_and_execution() {
    let mut campaign = TestCampaign::start().await;
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let root = campaign.root_installation.policy.clone();
    let result = campaign
        .drive_actor_output(
            &store,
            committed(
                root.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/notebook_prologue.hs",
                ),
            ),
        )
        .await;
    assert_eq!(explicit_display_output(&result)["text"], "True", "{result}");
    let later = campaign
        .drive_actor_output(
            &store,
            committed(
                root.as_ref(),
                "later <- pure (Imported.reverse [answer, 10])\n_ <- display (later == [10, 7])\n",
            ),
        )
        .await;
    assert_eq!(explicit_display_output(&later)["text"], "True", "{later}");
    let disabled = dispatch_haskell_script(
        root.as_ref(),
        "notInstalled <- pure (let ?offset = 5 in implicitTotal 2)\nnotInstalled\n",
    )
    .await;
    assert_eq!(
        disabled["status"], "rejected",
        "cell flags must not leak: {disabled}"
    );
    let imported = committed(
        root.as_ref(),
        "import qualified Data.Maybe as ImportedMaybe\n",
    )
    .await;
    assert_eq!(imported["items"][0]["kind"], "declaration", "{imported}");
    committed(
        root.as_ref(),
        "optional <- pure (ImportedMaybe.fromMaybe answer Nothing)\noptional\n",
    )
    .await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_rejection_retains_its_source_plan() {
    let campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let rejected = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/notebook_multiple_errors.hs",
        ),
    )
    .await;
    assert_eq!(rejected["status"], "rejected", "{rejected:?}");
    let items = rejected["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "{rejected:?}");
    assert_eq!(items[0]["status"], "rejected", "{rejected:?}");
    assert!(
        items[0]["output"].as_str().unwrap().contains("<cell>:2:"),
        "{rejected:?}"
    );
    assert_eq!(items[1]["status"], "notRun", "{rejected:?}");
    assert_eq!(items[2]["status"], "rejected", "{rejected:?}");
    assert!(
        items[2]["output"].as_str().unwrap().contains("<cell>:4:"),
        "{rejected:?}"
    );
    assert_eq!(items[3]["status"], "notRun", "{rejected:?}");
    assert!(
        items.iter().all(|item| item["kind"].is_string()
            && item["span"].is_object()
            && item["installedBindings"]
                .as_array()
                .is_none_or(Vec::is_empty)),
        "{rejected:?}"
    );
    let missing = dispatch_haskell_script(root.as_ref(), "willNotRun").await;
    assert_eq!(missing["status"], "rejected", "{missing:?}");
    assert!(missing.to_string().contains("not in scope"), "{missing:?}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_preserves_old_types() {
    let mut campaign = TestCampaign::start().await;
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/notebook_identity_original.hs",
        ),
    )
    .await;
    let shadowed = campaign
        .drive_actor_output(
            &store,
            committed(
                root.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/notebook_identity_shadowed.hs",
                ),
            ),
        )
        .await;
    let output = explicit_display_output(&shadowed)["text"].as_str().unwrap();
    assert!(
        output.contains("OldVersion 1") && output.contains("NewVersion True"),
        "{shadowed}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_retains_observations_needed_by_its_suffix() {
    let mut campaign = TestCampaign::start().await;
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let root = campaign.root_installation.policy.clone();
    let initial = committed(root.as_ref(), "(42 :: Int)\n").await;
    let saved = initial["items"][0]["installedBindings"][0]
        .as_str()
        .unwrap();
    let source =
        tidepool_testing::fixture_source("bridge/facade/src/actor_host/notebook_lease_suffix.hs")
            .replace("__SAVED__", saved);
    let result = campaign
        .drive_actor_output(&store, committed(root.as_ref(), &source))
        .await;
    assert_eq!(explicit_display_output(&result)["text"], "42", "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_infers_response_results() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let response = displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/notebook_identity_response.hs",
        ),
    )
    .await;
    let output = explicit_display_output(&response)["text"].as_str().unwrap();
    assert!(output.contains("Pending"), "{response:?}");
    committed(root.as_ref(), "later\npollResponse pending\n").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_reply_marks_its_tail_not_run() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        "worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent \"notebook-reply-worker\"))\n",
    )
    .await;
    committed(
        root.as_ref(),
        "response <- do { issued <- request @Text worker (assignment [label|notebook-reply|] (\"ready\" :: Text)); Right () <- detachRequest issued; pure issued }\n",
    )
    .await;
    let child = campaign
        .next_deployment(
            "notebook reply child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "notebook reply request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.message.contains("notebook-reply") =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;

    let reply = dispatch_haskell_script(
        child.policy.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/notebook_reply_tail.hs"),
    )
    .await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    assert_eq!(
        reply["summary"], "0 declarations, 1 statement, 1 expression",
        "{reply:?}"
    );
    let items = reply["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{reply:?}");
    assert_eq!(items[0]["status"], "committed", "{reply:?}");
    assert_eq!(items[0]["kind"], "expression", "{reply:?}");
    assert_eq!(items[0]["terminalTransfer"], "replyAccepted", "{reply:?}");
    assert_eq!(items[1]["status"], "notRun", "{reply:?}");
    assert_eq!(items[1]["kind"], "statement", "{reply:?}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

fn open_test_fork(
    campaign: &TestCampaign,
    child: &exomonad_actor::LocalResidentInstallation,
) -> Arc<dyn exomonad_actor::ForkWorkspaceCustody> {
    campaign.authority.install_grant(
        child.actor.identity().into(),
        worktree_grant(child.effective_role.role()),
    );
    let [worktree_id] = child.launch_worktrees.as_slice() else {
        panic!("child must have one worktree")
    };
    let worktree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(worktree_id))
        .unwrap()
        .unwrap();
    let principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(campaign.session_root.path()),
        child.actor.identity().id.0,
        child.actor.identity().incarnation.0,
    );
    assert_eq!(
        campaign
            .bindings
            .lock()
            .current(worktree.id())
            .unwrap()
            .agent(),
        &principal
    );
    let binding = child
        .worktree_custody
        .clone()
        .expect("bootstrap installed custody");
    child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    binding
}

#[tokio::test]
async fn published_unfold_watch_and_request_examples_execute() {
    execute_examples(false, None, 1).await;
}

#[tokio::test]
async fn record_actor_unfold_publishes_and_routes_child_reply() {
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(&authored).unwrap();
            crate::exomonad::write_fixture_project_config(&authored, "gpt-6-sol", |project| {
                project.haskell.source_roots = vec![".".into()];
                project.haskell.modules = vec!["LaunchFixture".into()];
            });
            std::fs::write(
                authored.join("LaunchFixture.hs"),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/record_actor_unfold.hs",
                ),
            )
            .unwrap();
            super::test_campaign::commit_workspace(&config.workspace);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        r#"import LaunchFixture
Right launcherTree <- createWorktree (fromRef (GitRef "HEAD") "resident-launcher")
launcher <- R.start (R.withWorktree (worktreeId launcherTree) launchDefinition)"#,
    )
    .await;
    committed(
        root.as_ref(),
        "R.send (launchAndAwait (R.client launcher)) ()",
    )
    .await;
    let child = campaign
        .next_deployment(
            "record actor's reviewer",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                LocalResidentDeployment::NotificationSend(notice) => {
                    panic!("unexpected launch notice: {}", notice.message())
                }
                other => Err(other),
            },
        )
        .await;
    assert!(
        child.fork_boundary.is_none(),
        "resident handler has no notebook completion"
    );
    let _custody = open_test_fork(&campaign, &child);
    tokio::time::timeout(
        Duration::from_secs(5),
        child.fork_gate.as_ref().unwrap().wait_committed(),
    )
    .await
    .expect("resident admission must publish without a notebook callback")
    .unwrap();
    campaign
        .next_deployment(
            "review request activation",
            Duration::from_secs(60),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == child.actor.identity() =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let reply = child
        .policy
        .dispatch_json_boxed(ToolInvocation {
            context: None,
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw("respond (\"review complete\" :: Text)".into()),
        })
        .await
        .unwrap();
    assert_eq!(reply["status"], "replied", "{reply}");
    let result = displayed(
        &mut campaign,
        root.as_ref(),
        "R.call (readReply (R.client launcher)) () >>= display",
    )
    .await;
    assert!(result.to_string().contains("review complete"), "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn invalid_label_literals_fail_before_actor_side_effects() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let me = displayed(
        &mut campaign,
        root.as_ref(),
        "display (inspectFull (agentIdentity me))",
    )
    .await;
    let identity = campaign.root_installation.actor.identity();
    assert_eq!(
        explicit_display_output(&me)["text"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .collect::<String>(),
        format!("({},{})", identity.id.0, identity.incarnation.0)
    );

    let invalid_watch = dispatch_haskell_script(
        root.as_ref(),
        "badWatch <- watch (\"Bad Label\" :: WatchLabel) (pure ())",
    )
    .await;
    assert_eq!(invalid_watch["status"], "rejected", "{invalid_watch}");
    assert!(
        invalid_watch
            .to_string()
            .contains("InvalidWatchLabel \\\"Bad Label\\\""),
        "{invalid_watch}"
    );
    for (source, expected) in [
        (
            "emptyWatch <- watch (\"\" :: WatchLabel) (pure ())",
            "EmptyWatchLabel",
        ),
        (
            concat!(
                "longWatch <- watch ",
                "(\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" :: WatchLabel) ",
                "(pure ())",
            ),
            "WatchLabelTooLong",
        ),
    ] {
        let rejected = dispatch_haskell_script(root.as_ref(), source).await;
        assert_eq!(rejected["status"], "rejected", "{rejected}");
        assert!(rejected.to_string().contains(expected), "{rejected}");
    }

    let invalid_unfold = dispatch_haskell_script(
        root.as_ref(),
        concat!(
            "badWorkers <- unfoldDeferred (batch \"literal-errors\" \"branches\") $ ",
            "(,) <$> child (withLifetime ActorOwned (researching @Text projectHead (assignment [label|valid|] ()))) ",
            "<*> child (withLifetime ActorOwned (researching @Text projectHead (assignment [label|Bad Label|] ())))",
        ),
    )
    .await;
    assert_eq!(invalid_unfold["status"], "rejected", "{invalid_unfold}");
    assert!(
        invalid_unfold
            .to_string()
            .contains("InvalidKebabName \\\"Bad Label\\\""),
        "{invalid_unfold}"
    );
    campaign.assert_no_deployment(
        "an invalid later branch launched an earlier child",
        |event| matches!(event, LocalResidentDeployment::PolicyInstalled(_)),
    );

    committed(
        root.as_ref(),
        concat!(
            "worker <- unfoldDeferred (batch \"literal-errors\" \"request\") ",
            "(child (withLifetime ActorOwned (researching @Text projectHead (assignment [label|target|] ()))))",
        ),
    )
    .await;
    let (_worker_installation, _worker_custody) = next_project_worker(&mut campaign).await;
    committed(root.as_ref(), "before <- listAgents").await;
    committed(
        root.as_ref(),
        concat!(
            "let requestCount target rows = sum [length (rosterCurrentRequests row) | ",
            "row <- rows, (rosterActorId row, rosterActorIncarnation row) == target]",
        ),
    )
    .await;
    committed(
        root.as_ref(),
        "let beforeRequests = requestCount (agentIdentity (responseActor worker)) before",
    )
    .await;
    let rendered = displayed(&mut campaign, root.as_ref(), "display (inspectFull worker)").await;
    let rendered = explicit_display_output(&rendered)["text"].as_str().unwrap();
    assert!(rendered.starts_with("<response to request "), "{rendered}");
    assert!(rendered.contains(" from agent "), "{rendered}");
    assert!(rendered.contains(", path "), "{rendered}");
    assert!(!rendered.contains("AgentRef"), "{rendered}");
    let invalid_request = dispatch_haskell_script(
        root.as_ref(),
        "badResponse <- request @Text (responseActor worker) (assignment [label|Bad Label|] ())",
    )
    .await;
    assert_eq!(invalid_request["status"], "rejected", "{invalid_request}");
    assert!(
        invalid_request
            .to_string()
            .contains("InvalidKebabName \\\"Bad Label\\\""),
        "{invalid_request}"
    );
    committed(root.as_ref(), "after <- listAgents").await;
    let unchanged = displayed(
        &mut campaign,
        root.as_ref(),
        "display (requestCount (agentIdentity (responseActor worker)) after == beforeRequests)",
    )
    .await;
    assert_eq!(
        explicit_display_output(&unchanged)["text"],
        "True",
        "{unchanged}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn shared_api_guide_example_handles_success_and_unavailable() {
    // The guide's command/judgment example reads `J`, which a run gets from the
    // Jev library its workspace pins, so this campaign selects that workspace.
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        super::test_campaign::pinned_jev_workspace,
    )
    .await;
    let root = campaign.root_installation.policy.clone();
    let guide = include_str!("../../../../exomonad/prompts/api-guide.md");
    let mut guide_examples = examples(guide);
    displayed(&mut campaign, root.as_ref(), guide_examples.next().unwrap()).await;
    let child = campaign
        .next_deployment(
            "guide example child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    let _binding = open_test_fork(&campaign, &child);
    campaign
        .next_deployment(
            "guide example session readiness",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation } => {
                    assert!(activation.message.contains("Remove the stale path"));
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let pending = displayed(
        &mut campaign,
        root.as_ref(),
        "state <- pollWatch ready\ndisplay (inspectFull state)",
    )
    .await;
    let rendered = explicit_display_output(&pending)["text"].as_str().unwrap();
    // A pending watch observation is itself the registered wake: its
    // `PendingProgress` carries the dependency's lifecycle/provider evidence
    // and `pendingWatched = True`, so `inspectFull` shows there is nothing a
    // re-poll would add.
    assert!(rendered.contains("WatchPending"), "{rendered}");
    assert!(rendered.contains("PendingProgress"), "{rendered}");
    assert!(rendered.contains("pendingActorState"), "{rendered}");
    assert!(rendered.contains("pendingProviderHealth"), "{rendered}");
    assert!(rendered.contains("pendingWatched = True"), "{rendered}");
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    let success = displayed(
        &mut campaign,
        root.as_ref(),
        "state <- pollWatch ready\ndisplay (inspectFull (fmap settledValue state))",
    )
    .await;
    displayed(&mut campaign, root.as_ref(), guide_examples.next().unwrap()).await;
    displayed(&mut campaign, root.as_ref(), guide_examples.next().unwrap()).await;
    let command_example = guide_examples.next().unwrap();
    let lookup_example = guide_examples.next().unwrap();
    assert!(guide_examples.next().is_none(), "untested guide example");
    displayed(&mut campaign, root.as_ref(), lookup_example).await;
    let topics_found = displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/lookup_topics_found.hs"),
    )
    .await;
    assert_eq!(
        explicit_display_output(&topics_found)["text"],
        "True",
        "{topics_found}"
    );
    displayed(
        &mut campaign,
        root.as_ref(),
        example(include_str!(
            "../../../../.exomonad/workspace/skills/exomonad-jev/references/recent-changes.md"
        )),
    )
    .await;
    // Exercise the canonical command/Jev composition and workbench examples.
    // The host requests a backend for each command in a cell.
    let command_examples = [
        "inspectRecentChanges \"inspect changed documentation\" >>= display",
        command_example,
        examples(include_str!(
            "../../../../.exomonad/workspace/skills/exomonad-workbench/SKILL.md"
        ))
        .nth(2)
        .unwrap(),
        examples(include_str!(
            "../../../../exomonad/examples/workspace/.exomonad/skills/exomonad-workbench/SKILL.md"
        ))
        .nth(4)
        .unwrap(),
        example(include_str!(
            "../../../../.exomonad/workspace/skills/exomonad-jev/SKILL.md"
        )),
    ];
    let output_store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let output_run = super::runtime_namespace(campaign.session_root.path());
    for (index, source) in command_examples.into_iter().enumerate() {
        let mut running = tokio::spawn({
            let root = root.clone();
            async move { dispatch_haskell_script(root.as_ref(), source).await }
        });
        let result = loop {
            tokio::select! {
                result = &mut running => break result.unwrap(),
                // Compile and reload the Haskell cell before its command
                // backend deployment arrives; this campaign can exceed the
                // generic 30-second command-only helper timeout.
                request = campaign.next_deployment(
                    "guide example command backend",
                    Duration::from_secs(180),
                    |event| match event {
                        LocalResidentDeployment::CommandBackend(_) | LocalResidentDeployment::DisplayPublished(_) => Ok(event),
                        other => Err(other),
                    },
                ) => {
                    let request = match request {
                        LocalResidentDeployment::CommandBackend(request) => request,
                        LocalResidentDeployment::DisplayPublished(request) => {
                            super::display_output::publish(&campaign.forest, &output_store, &output_run, None, None, &request);
                            continue;
                        }
                        _ => unreachable!(),
                    };
                    if request.purpose
                        == exomonad_actor::command_jobs::CommandBackendPurpose::SourceProbe
                    {
                        request.supply(Ok(super::command_test_support::TestCommands::completed(
                            "/work/tree\n0123456789abcdef0123456789abcdef01234567\nclean\n",
                        )));
                        continue;
                    }
                    request.supply(Ok(super::command_test_support::TestCommands::completed("README.md")));
                }
            }
        };
        assert_eq!(result["status"], "committed", "{source}: {result}");
        if index == 0 {
            assert!(result.to_string().contains("Jev unavailable:"), "{result}");
        }
    }
    let layout = displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/notebook_let_layout.hs"),
    )
    .await;
    assert_eq!(explicit_display_output(&layout)["text"], "42", "{layout}");
    assert_eq!(
        explicit_display_output(&success)["text"],
        "WatchReady (Right \"Remove the stale path and report the focused check.\")"
    );

    displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/shared_api_guide_unavailable.hs",
        ),
    )
    .await;
    campaign.await_watch_ready().await;
    let unavailable = displayed(
        &mut campaign, root.as_ref(),
        "state <- pollWatch retainedFailureReady\ndisplay (inspectFull (fmap (either (const True) (const False) . settledValue) state))",
    )
    .await;
    assert_eq!(
        explicit_display_output(&unavailable)["text"],
        "WatchReady True"
    );
    let outer_unavailable = displayed(
        &mut campaign,
        root.as_ref(),
        "state <- pollWatch outerFailureReady\ndisplay (inspectFull (guideIsUnavailable state))",
    )
    .await;
    assert_eq!(explicit_display_output(&outer_unavailable)["text"], "True");

    // The guide's companion `doc reflect` example, on this same campaign so it
    // needs no compile of its own. This root has no conversation reader, which
    // is the unbound case the example is written to survive: it continues with
    // no history rather than being handed somebody else's.
    let reflect = displayed(
        &mut campaign,
        root.as_ref(),
        example(include_str!("../../../../exomonad/prompts/docs/reflect.md")),
    )
    .await;
    assert_eq!(explicit_display_output(&reflect)["text"], "[]", "{reflect}");
    assert_eq!(
        reflect["items"][1]["operations"][0]["effect"], "reflect",
        "{reflect}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn watch_documentation_request_options_reports_progress_then_settles() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        "lead <- startAgent (withAgentLifetime ActorOwned (readonlyAgent \"documented-progress-lead\"))",
    )
    .await;
    let snippets: Vec<_> = examples(include_str!("../../../../exomonad/prompts/docs/watch.md"))
        .filter(|snippet| snippet.contains("let progressOptions = assignment"))
        .collect();
    assert_eq!(snippets.len(), 1, "one complete documented progress setup");
    displayed(&mut campaign, root.as_ref(), snippets[0]).await;
    let child = campaign
        .next_deployment(
            "documented progress child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "documented progress request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation } => {
                    assert!(activation
                        .message
                        .contains("Publish cumulative findings; then return your final report."));
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    // Exercise the real resident actor path, without a model/provider execution claim.
    committed(child.policy.as_ref(), "reportProgress [\"finding\"]").await;
    campaign.await_watch_ready().await;
    let progress = displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/watch_documentation_progress.hs",
        ),
    )
    .await;
    let pages: Vec<_> = progress["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["operations"].as_array().unwrap())
        .filter_map(|operation| operation.get("display").map(|display| &display["text"]))
        .collect();
    assert_eq!(
        pages,
        vec![&serde_json::json!("True"), &serde_json::json!("True")],
        "{progress}"
    );
    let reply =
        dispatch_haskell_script(child.policy.as_ref(), "respond (\"final report\" :: Text)").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    let settled = displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/watch_documentation_settled.hs",
        ),
    )
    .await;
    let pages: Vec<_> = settled["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["operations"].as_array().unwrap())
        .filter_map(|operation| operation.get("display").map(|display| &display["text"]))
        .collect();
    assert_eq!(
        pages,
        vec![&serde_json::json!("True"), &serde_json::json!("True")],
        "{settled}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn activation_presents_prose_and_preserves_exact_inputs() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let mut child = None;
    committed(root.as_ref(), "data Report = Report Int deriving Show\nworker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent \"activation-preview-worker\"))").await;
    committed(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host_fixtures/generic_actor/activation_preview_setup.hs",
        ),
    )
    .await;
    for (label, input, expected, reply) in [
        (
            "preview-text",
            "textPreview",
            "first line\nλ second line",
            "respond (Report 1)",
        ),
        (
            "preview-long-text",
            "longTextPreview",
            "FINAL-ACCEPTANCE-CONDITION",
            "respond (Report 1)",
        ),
        (
            "preview-task",
            "taskPreview",
            "STRUCTURED-ACCEPTANCE-TAIL",
            "respond (Report 1)",
        ),
        (
            "preview-oversized-text",
            "oversizedTextPreview",
            "past the 32 KiB cap; expand with `display sessionInput`",
            "respond (Report 1)",
        ),
        (
            "preview-opaque",
            "opaquePreview",
            "<function>",
            "respond (Report (sessionInput 16))",
        ),
        (
            "preview-effect",
            "effectPreview",
            "<opaque value>",
            "respond (Report 1)",
        ),
        (
            "preview-failure",
            "brokenPreview",
            "rendering unavailable",
            "case sessionInput of BrokenPreview n -> respond (Report n)",
        ),
    ] {
        committed(root.as_ref(), &format!("let previewLabel = [label|{label}|]\npreviewResponse <- do {{ issued <- request @Report worker (assignment previewLabel {input}); Right () <- detachRequest issued; pure issued }}")).await;
        let activation = campaign
            .next_deployment(
                "preview activation",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(installation) => {
                        child = Some((*installation).clone());
                        Err(LocalResidentDeployment::PolicyInstalled(installation))
                    }
                    LocalResidentDeployment::SessionReady { activation }
                        if activation.message.contains(label) =>
                    {
                        Ok(activation)
                    }
                    other => Err(other),
                },
            )
            .await;
        assert!(activation
            .message
            .contains("`reportProgress` is unavailable"));
        if label == "preview-text" {
            let unavailable =
                dispatch_lookup(child.as_ref().unwrap().policy.as_ref(), &["reportProgress"]).await;
            assert!(
                unavailable.to_string().contains("no match"),
                "{unavailable}"
            );
        }
        assert!(
            activation.message.contains(expected),
            "{}",
            activation.message
        );
        assert!(
            activation.message.contains("data Report"),
            "{}",
            activation.message
        );
        if label == "preview-long-text" || label == "preview-task" {
            assert!(
                !activation.message.contains("omitted"),
                "{}",
                activation.message
            );
        }
        if label == "preview-oversized-text" {
            assert!(activation.message.len() < 33 * 1024);
            assert!(!activation.message.contains("RETAINED-ASSIGNMENT-TAIL"));
            let retained = displayed(
                &mut campaign,
                child.as_ref().unwrap().policy.as_ref(),
                "display (T.isSuffixOf \"RETAINED-ASSIGNMENT-TAIL\" sessionInput)",
            )
            .await;
            assert_eq!(
                explicit_display_output(&retained)["text"],
                "True",
                "{retained}"
            );
        }
        let result = dispatch_haskell_script(child.as_ref().unwrap().policy.as_ref(), reply).await;
        assert_eq!(result["status"], "replied", "{result:?}");
    }
    committed(root.as_ref(), "stopAgent worker").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn rich_response_survives_resident_computation() {
    execute_examples(true, None, 1).await;
}

#[tokio::test]
async fn quiet_observation_retains_exact_results_without_repeating_effects() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/quiet_observation_setup.hs",
        ),
    )
    .await;
    let child = campaign
        .next_deployment(
            "quiet observation child policy installation",
            Duration::from_secs(60),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "quiet observation session readiness",
            Duration::from_secs(60),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond delivery").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    let first = committed(root.as_ref(), "pollWatch ready").await;
    let saved = first["items"][0]["installedBindings"][0].as_str().unwrap();
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let shown = campaign
        .drive_actor_output(
            &store,
            committed(root.as_ref(), &format!("display ({saved} ())")),
        )
        .await;
    let output = explicit_display_output(&shown)["text"].as_str().unwrap();
    assert!(output.starts_with("WatchReady"), "{shown}");
    assert!(
        !explicit_display_output(&shown)["expansions"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{shown}"
    );
    let expanded = campaign
        .drive_actor_output(
            &store,
            committed(
                root.as_ref(),
                &format!("display (inspectFull ({saved} ()))"),
            ),
        )
        .await;
    assert_eq!(
        explicit_display_output(&expanded)["text"],
        output,
        "{expanded}"
    );
    assert_eq!(
        explicit_display_output(&expanded)["expansions"],
        explicit_display_output(&shown)["expansions"],
        "{expanded}"
    );
    committed(root.as_ref(), &format!("let retained = {saved} ()")).await;
    committed(root.as_ref(), &format!("declaredEvidence = {saved} ()")).await;
    let second = committed(root.as_ref(), "pollWatch ready").await;
    assert_ne!(
        first["items"][0]["installedBindings"],
        second["items"][0]["installedBindings"]
    );
    let expiring = second["items"][0]["installedBindings"][0].as_str().unwrap();
    let failed_declaration = dispatch_haskell_script(
        root.as_ref(),
        "brokenDeclaration = missingObservationDependency :: Int",
    )
    .await;
    assert_eq!(
        failed_declaration["status"], "rejected",
        "{failed_declaration}"
    );
    for _ in 0..9 {
        committed(root.as_ref(), "pollWatch ready").await;
    }
    let expired = dispatch_haskell_script(root.as_ref(), &format!("{expiring} ()")).await;
    assert_eq!(expired["status"], "rejected", "{expired}");
    for name in ["retained", "declaredEvidence"] {
        let source = tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/quiet_observation_exact_probe.hs",
        )
        .replace("__NAME__", name);
        let exact = campaign
            .drive_actor_output(&store, committed(root.as_ref(), &source))
            .await;
        assert_eq!(explicit_display_output(&exact)["text"], "True", "{exact}");
    }
    let before = campaign
        .forest
        .inspect_graph(campaign.actor.identity())
        .unwrap()
        .len();
    let spawned = committed(
        root.as_ref(),
        "startAgent (withAgentLifetime ActorOwned (readonlyAgent \"observe-once\"))",
    )
    .await;
    let spawned_name = spawned["items"][0]["installedBindings"][0]
        .as_str()
        .unwrap();
    let inspect = format!("display (inspectFull (agentIdentity ({spawned_name} ())))");
    let one = campaign
        .drive_actor_output(&store, committed(root.as_ref(), &inspect))
        .await;
    let two = campaign
        .drive_actor_output(&store, committed(root.as_ref(), &inspect))
        .await;
    assert_eq!(
        explicit_display_output(&one)["text"],
        explicit_display_output(&two)["text"]
    );
    assert_eq!(
        campaign
            .forest
            .inspect_graph(campaign.actor.identity())
            .unwrap()
            .len(),
        before + 1
    );
    committed(root.as_ref(), &format!("stopAgent ({spawned_name} ())")).await;
    committed(root.as_ref(), "stopAgent worker").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn queued_unfold_survives_later_rejection() {
    execute_examples(false, Some("missingBindingAfterSuccessfulUnfold"), 1).await;
}

#[tokio::test]
async fn reattachment_preserves_completed_unacknowledged_forks() {
    let mut campaign = TestCampaign::start().await;
    let root = Arc::clone(&campaign.root_installation.policy);
    committed(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host_fixtures/generic_actor/documentation_setup.hs",
        ),
    )
    .await;
    let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
        "actor-host-recovery".into(),
        "minimal-turn".into(),
        "minimal-unfold".into(),
    );
    let result = root
        .dispatch_json_boxed(ToolInvocation {
            context: Some(ToolInvocationContext::external(
                boundary.hosted().unwrap().external_thread().unwrap().into(),
                "minimal-turn".into(),
                "minimal-inner-call".into(),
                Some(boundary.hosted().unwrap().call_id.clone()),
                Some("haskell".into()),
            )),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host_fixtures/generic_actor/deferred_unfold_example.hs",
            )),
        })
        .await
        .unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    campaign.assert_no_deployment("no child before reattachment", |event| {
        matches!(event, LocalResidentDeployment::PolicyInstalled(_))
    });
    root.reattach_boxed().await.unwrap();
    assert!(matches!(
        root.reconcile_workbench_boxed(boundary.clone())
            .await
            .unwrap(),
        exomonad_actor::WorkbenchBoundaryReconciliation::Recovered { .. }
    ));
    root.complete_boxed(boundary.clone()).await.unwrap();
    root.complete_boxed(boundary.clone()).await.unwrap();

    let mut children = Vec::new();
    while children.len() < 2 {
        let child = campaign
            .next_deployment(
                "recovered fork startup",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                    other => Err(other),
                },
            )
            .await;
        assert_eq!(child.fork_boundary.as_ref(), Some(&boundary));
        children.push(child);
    }
    for child in &children {
        let inherited = committed(child.policy.as_ref(), "sessionInput").await;
        assert!(inherited["items"][0]["output"].as_str().is_some());
    }
    campaign.assert_no_deployment(
        "exactly two children belong to the recovered unfold",
        |event| matches!(event, LocalResidentDeployment::PolicyInstalled(_)),
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn multiple_unfolds_are_admitted_before_completion() {
    execute_examples(false, None, 2).await;
}

async fn execute_examples(rich_response: bool, suffix: Option<&str>, groups: usize) {
    let mut campaign = TestCampaign::start().await;
    let root = Arc::clone(&campaign.root_installation.policy);
    for block in include_str!("../../../../exomonad/prompts/docs/workbench.md")
        .split("```haskell\n")
        .skip(1)
    {
        displayed(
            &mut campaign,
            root.as_ref(),
            block.split_once("```").unwrap().0,
        )
        .await;
    }
    let setup = if rich_response {
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host_fixtures/generic_actor/rich_response_setup.hs",
        )
    } else {
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host_fixtures/generic_actor/documentation_setup.hs",
        )
    };
    displayed(&mut campaign, root.as_ref(), &setup).await;
    let review_type = dispatch_lookup(root.as_ref(), &["Review"]).await;
    assert_eq!(review_type["status"], "committed", "{review_type:?}");
    let extra_group = if groups == 2 {
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host_fixtures/generic_actor/second_queued_unfold.hs",
        )
    } else {
        String::new()
    };
    let call_id = "documentation-unfold".to_owned();
    let (invocation, completion) = original_tool_call(OriginalOperation {
        origin: ConversationOrigin::External {
            thread_id: "actor-host-vertical".into(),
        },
        request_id: call_id.clone(),
        call_id,
    });
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        root.dispatch_json_boxed(ToolInvocation {
            context: Some(invocation),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(format!(
                "{}\n{}\n{}\n{}",
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host_fixtures/generic_actor/deferred_unfold_example.hs",
                ),
                example(include_str!("../../../../exomonad/prompts/docs/watch.md")),
                extra_group,
                suffix.unwrap_or("")
            )),
        }),
    )
    .await
    .expect("deferred unfold must return without provider startup")
    .unwrap();
    assert_eq!(
        result["status"],
        if suffix.is_some() {
            "rejected"
        } else {
            "committed"
        },
        "{result:?}"
    );
    campaign.assert_no_deployment("child started before tool completion", |event| {
        matches!(event, LocalResidentDeployment::PolicyInstalled(_))
    });

    let expected_children = groups * 2;
    root.complete_boxed(completion.clone()).await.unwrap();
    root.complete_boxed(completion).await.unwrap();
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    let mut fork_boundary = None;
    let mut activation_messages = Vec::new();
    enum Collected {
        Child(Box<exomonad_actor::LocalResidentInstallation>),
        Activation(exomonad_actor::ResidentActivation),
    }
    while children.len() < expected_children {
        let collected = campaign
            .next_deployment(
                "child deployment",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(child) => Ok(Collected::Child(child)),
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        panic!("{actor:?} retired: {terminal:?}")
                    }
                    LocalResidentDeployment::SessionReady { activation } => {
                        Ok(Collected::Activation(activation))
                    }
                    other => Err(other),
                },
            )
            .await;
        match collected {
            Collected::Child(child) => {
                let expected_effort = if child.label.ends_with("/consumer-tests") {
                    Some(exomonad_actor::ForkEffort::Medium)
                } else if child.label.ends_with("/later-wave/domain") {
                    Some(exomonad_actor::ForkEffort::Low)
                } else {
                    None
                };
                assert_eq!(
                    child.fork_effort, expected_effort,
                    "explicit effort is preserved; omitted effort stays a host default"
                );
                let boundary = child.fork_boundary.as_ref().expect("hosted fork boundary");
                let operation = boundary.hosted().expect("hosted operation");
                assert_eq!(operation.external_thread(), Some("actor-host-vertical"));
                assert!(!operation.call_id.is_empty());
                if let Some(expected) = &fork_boundary {
                    assert_eq!(
                        boundary, expected,
                        "siblings inherit the same hosted invocation"
                    );
                } else {
                    fork_boundary = Some(boundary.clone());
                }
                bindings.push(open_test_fork(&campaign, &child));
                children.push(child);
            }
            Collected::Activation(activation) => activation_messages.push(activation),
        }
    }
    let mut presented = std::collections::HashSet::new();
    while presented.len() < children.len() {
        let activation =
            if let Some(activation) = activation_messages.pop() {
                activation
            } else {
                campaign
                    .next_deployment("request activation", Duration::from_secs(120), |event| {
                        match event {
                            LocalResidentDeployment::SessionReady { activation } => Ok(activation),
                            other => Err(other),
                        }
                    })
                    .await
            };
        let Some(child) = children
            .iter()
            .find(|child| child.actor.identity() == activation.id.actor())
        else {
            continue;
        };
        assert!(
            !activation.message.contains("rendering unavailable"),
            "{}",
            activation.message
        );
        if !rich_response && child.label.ends_with("/domain") {
            assert!(
                activation.message.contains("sessionInput :: Int`):\n\n7"),
                "{}",
                activation.message
            );
            assert!(
                activation.message.contains("data Report"),
                "{}",
                activation.message
            );
        }
        presented.insert(activation.id.actor());
    }
    displayed(
        &mut campaign,
        root.as_ref(),
        "let sharedAfterUnfold = (\"later parent value\" :: Text)",
    )
    .await;
    for child in &children {
        let inherited = displayed(
            &mut campaign,
            child.policy.as_ref(),
            "display sharedAfterUnfold",
        )
        .await;
        assert_eq!(
            explicit_display_output(&inherited)["text"]
                .as_str()
                .unwrap()
                .trim(),
            "ready"
        );
    }
    displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host_fixtures/generic_actor/response_computation.hs",
        ),
    )
    .await;
    for child in &children {
        let source = if rich_response {
            tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host_fixtures/generic_actor/rich_response_reply.hs",
            )
        } else if child.label.ends_with("/domain") {
            "respond (Report sessionInput)".to_owned()
        } else {
            "respond sessionInput".to_owned()
        };
        let result = dispatch_haskell_script(child.policy.as_ref(), &source).await;
        assert_eq!(result["status"], "replied", "{result:?}");
    }
    campaign.await_watch_ready().await;
    let first = displayed(
        &mut campaign,
        root.as_ref(),
        "firstState <- pollWatch joined\ndisplay firstState",
    )
    .await;
    let second = displayed(
        &mut campaign,
        root.as_ref(),
        "secondState <- pollWatch joined\ndisplay secondState",
    )
    .await;
    for observation in [&first, &second] {
        assert!(explicit_display_output(observation)["text"]
            .as_str()
            .unwrap()
            .starts_with("WatchReady"));
    }
    let full = displayed(
        &mut campaign,
        root.as_ref(),
        "display (inspectFull firstState)",
    )
    .await;
    assert!(explicit_display_output(&full)["text"]
        .as_str()
        .unwrap()
        .contains("ReplyAvailable"));
    displayed(
        &mut campaign, root.as_ref(),
        "import Tidepool.Actors.Observe (actorContext)\ncontext <- actorContext\ncontextFirstUsage context\ncontextLatestUsage context",
    )
    .await;
    displayed(
        &mut campaign,
        root.as_ref(),
        "let worker = responseActor (fst workers)\nlet task = 9 :: Int",
    )
    .await;
    let domain = children
        .iter()
        .find(|child| child.label.ends_with("/domain"))
        .unwrap();
    for document in [
        include_str!("../../../../exomonad/prompts/docs/request.md"),
        include_str!("../../../../exomonad/prompts/docs/deadline.md"),
    ] {
        if rich_response {
            break;
        }
        displayed(&mut campaign, root.as_ref(), example(document)).await;
        displayed(
            &mut campaign, root.as_ref(),
            "let readyLabel = \"followup-result\" :: WatchLabel\nready <- watch readyLabel (awaitResponse response)",
        )
        .await;
        let reply =
            dispatch_haskell_script(domain.policy.as_ref(), "respond (Report sessionInput)").await;
        assert_eq!(reply["status"], "replied", "{reply:?}");
        campaign.await_watch_ready().await;
        let result = displayed(
            &mut campaign,
            root.as_ref(),
            "responseState <- pollResponse response\ndisplay responseState",
        )
        .await;
        assert!(explicit_display_output(&result)["text"]
            .as_str()
            .unwrap()
            .starts_with("ResponseReady"));
        let full = displayed(&mut campaign, root.as_ref(), "display (show responseState)").await;
        assert!(
            explicit_display_output(&full)["text"]
                .as_str()
                .unwrap()
                .contains("Report 9"),
            "{full}"
        );
    }
    for child in children {
        child
            .actor
            .shutdown(ActorTerminal {
                kind: if child.label.ends_with("/consumer-tests") {
                    ActorExitKind::Failed
                } else {
                    ActorExitKind::Cancelled
                },
                summary: "documentation scenario terminal evidence".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
    }
    let status = dispatch_status(root.as_ref(), "summary").await;
    let status = status["items"][0]["output"].as_str().unwrap();
    assert!(
        status.contains("terminal:Failed")
            && status.contains("documentation scenario terminal evidence"),
        "{status}"
    );
    campaign
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "documentation scenario complete".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn model_selection_is_independent_of_inherited_and_selected_context() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/fixtures/model_context.hs"),
    )
    .await;
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    {
        enum Arrival {
            Child(Box<exomonad_actor::LocalResidentInstallation>),
            Ready,
        }
        let mut ready = 0;
        while ready != 2 {
            let arrival = campaign
                .next_deployment(
                    "model-context child admission",
                    Duration::from_secs(120),
                    |event| match event {
                        LocalResidentDeployment::PolicyInstalled(child) => {
                            Ok(Arrival::Child(child))
                        }
                        LocalResidentDeployment::SessionReady { .. } => Ok(Arrival::Ready),
                        other => Err(other),
                    },
                )
                .await;
            match arrival {
                Arrival::Child(child) => {
                    bindings.push(open_test_fork(&campaign, &child));
                    children.push(child);
                }
                Arrival::Ready => ready += 1,
            }
        }
    }
    for child in &children {
        assert_eq!(child.model.as_deref(), Some("gpt-6-sol"));
        assert_eq!(child.supervisor_parent, Some(campaign.actor.identity()));
        if child.label.ends_with("/exact") {
            assert_eq!(child.context_parent, Some(campaign.actor.identity()));
            let inherited = displayed(
                &mut campaign,
                child.policy.as_ref(),
                "display (inspectFull parentOnly)",
            )
            .await;
            assert_eq!(explicit_display_output(&inherited)["text"], "41");
        } else {
            assert_eq!(child.context_parent, None);
            assert_eq!(child.fork_effort, Some(exomonad_actor::ForkEffort::Medium));
            let missing = dispatch_lookup(child.policy.as_ref(), &["parentOnly"]).await;
            assert!(missing.to_string().contains("no match"), "{missing}");
        }
        let reply = dispatch_haskell_script(child.policy.as_ref(), "respond sessionInput").await;
        assert_eq!(reply["status"], "replied", "{reply}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn routes_forward_without_model_relay_and_retain_callback_failure() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/fixtures/route.hs"),
    )
    .await;
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    {
        enum Arrival {
            Child(Box<exomonad_actor::LocalResidentInstallation>),
            Ready,
        }
        let mut ready = 0;
        while ready != 2 {
            let arrival = campaign
                .next_deployment("route child admission", Duration::from_secs(120), |event| {
                    match event {
                        LocalResidentDeployment::PolicyInstalled(child) => {
                            Ok(Arrival::Child(child))
                        }
                        LocalResidentDeployment::SessionReady { .. } => Ok(Arrival::Ready),
                        other => Err(other),
                    }
                })
                .await;
            match arrival {
                Arrival::Child(child) => {
                    bindings.push(open_test_fork(&campaign, &child));
                    children.push(child);
                }
                Arrival::Ready => ready += 1,
            }
        }
    }
    let consumer = children
        .iter()
        .find(|child| child.label.ends_with("/consumer"))
        .unwrap();
    let producer = children
        .iter()
        .find(|child| child.label.ends_with("/producer"))
        .unwrap();
    dispatch_haskell_script(consumer.policy.as_ref(), "respond sessionInput").await;
    dispatch_haskell_script(producer.policy.as_ref(), "respond sessionInput").await;
    let reviewer = {
        enum ReviewArrival {
            Child(Box<exomonad_actor::LocalResidentInstallation>),
            Ready { message: String },
            RouteFailure { detail: String },
        }
        let owner = campaign.actor.identity();
        let mut reviewer = None;
        let mut forwarded = false;
        let mut review_ready = false;
        let mut failure_notified = false;
        loop {
            let arrival = campaign
                .next_deployment(
                    "reviewer admission/activation",
                    Duration::from_secs(120),
                    |event| match event {
                        LocalResidentDeployment::PolicyInstalled(child) => {
                            Ok(ReviewArrival::Child(child))
                        }
                        LocalResidentDeployment::SessionReady { activation } => {
                            Ok(ReviewArrival::Ready {
                                message: activation.message,
                            })
                        }
                        LocalResidentDeployment::WatchChanged { notification }
                            if notification.owner == owner =>
                        {
                            let exomonad_actor::WatchTransition::RouteFailed { detail } =
                                notification.transition
                            else {
                                panic!("successful route woke a model: {notification:?}");
                            };
                            Ok(ReviewArrival::RouteFailure { detail })
                        }
                        other => Err(other),
                    },
                )
                .await;
            match arrival {
                ReviewArrival::Child(child) => {
                    assert_eq!(child.context_parent, None);
                    assert_eq!(child.model.as_deref(), Some("gpt-6-sol"));
                    bindings.push(open_test_fork(&campaign, &child));
                    reviewer = Some(child);
                }
                ReviewArrival::Ready { message } => {
                    if message.contains("review-candidate") {
                        forwarded = true;
                    } else {
                        review_ready = true;
                    }
                }
                ReviewArrival::RouteFailure { detail } => {
                    assert!(detail.contains("deliberate route failure"), "{detail}");
                    assert!(!failure_notified, "route failure notified twice");
                    failure_notified = true;
                }
            }
            if forwarded && review_ready && failure_notified {
                if let Some(reviewer) = reviewer.take() {
                    break reviewer;
                }
            }
        }
    };
    let review = dispatch_haskell_script(reviewer.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(review["status"], "replied", "{review}");
    let state = displayed(
        &mut campaign,
        root.as_ref(),
        "_ <- pollRoute forwarding >>= display . show\n_ <- pollRoute broken >>= display . show",
    )
    .await;
    assert_eq!(explicit_display_texts(&state)[0], "RouteCompleted");
    assert!(
        explicit_display_texts(&state)[1].contains("deliberate route failure"),
        "{state}"
    );
    let recovered = displayed(&mut campaign, root.as_ref(), "recovered <- listRoutes\n_ <- display $ inspectFull (length recovered)\nstates <- traverse pollRoute recovered\n_ <- display (show states)").await;
    assert_eq!(explicit_display_texts(&recovered)[0], "3", "{recovered}");
    assert!(
        explicit_display_texts(&recovered)[1].contains("deliberate route failure"),
        "{recovered}"
    );
    let foreign = displayed(
        &mut campaign,
        consumer.policy.as_ref(),
        "owned <- listRoutes\n_ <- display $ inspectFull (length owned)",
    )
    .await;
    assert_eq!(explicit_display_texts(&foreign)[0], "0", "{foreign}");
    let reply = dispatch_haskell_script(consumer.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    displayed(
        &mut campaign,
        root.as_ref(),
        "stopAgent (responseActor producer)",
    )
    .await;
    displayed(&mut campaign, root.as_ref(), "unavailable <- request @Text (responseActor producer) (assignment forwardedLabel (\"lost target\" :: Text))\nhandled <- route (awaitSettled unavailable) (\\settled -> case settled of { ReplyUnavailable _ -> pure (); ReplyAvailable _ -> error \"unexpected success\" })").await;
    let handled = displayed(
        &mut campaign, root.as_ref(),
        "_ <- pollRoute handled >>= display . show\n_ <- forgetRoute forwarding >>= display . show\n_ <- forgetRoute broken >>= display . show",
    )
    .await;
    assert_eq!(
        explicit_display_texts(&handled)[0],
        "RouteCompleted",
        "{handled}"
    );
    assert_eq!(
        explicit_display_texts(&handled)[1],
        "WatchForgotten",
        "{handled}"
    );
    assert_eq!(
        explicit_display_texts(&handled)[2],
        "WatchForgotten",
        "{handled}"
    );
    let forgotten = displayed(
        &mut campaign,
        root.as_ref(),
        "_ <- pollRoute broken >>= display . show",
    )
    .await;
    assert_eq!(
        explicit_display_texts(&forgotten)[0],
        "RouteRejected ReplyStale",
        "{forgotten}"
    );
    let retained = displayed(
        &mut campaign,
        root.as_ref(),
        "retained <- listRoutes\n_ <- display $ inspectFull (length retained)",
    )
    .await;
    assert_eq!(explicit_display_texts(&retained)[0], "2", "{retained}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn configured_modules_are_available_to_resident_declarations_from_frozen_sources() {
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            crate::exomonad::write_fixture_project_config(&authored, "gpt-6-sol", |project| {
                project.haskell.source_roots = vec![".".into()];
                project.haskell.modules = vec!["Project.Types".into(), "Project.Work".into()];
            });
            std::fs::write(
                authored.join("Project/Types.hs"),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/project/Types.hs",
                ),
            )
            .unwrap();
            std::fs::write(
                authored.join("Project/Work.hs"),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/project/Work.hs",
                ),
            )
            .unwrap();
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
            std::fs::write(authored.join("Project/Work.hs"), "invalid edited source").unwrap();
        },
    )
    .await;
    let policy = campaign.root_installation.policy.clone();
    let result = displayed(
        &mut campaign,
        policy.as_ref(),
        "saved <- pure candidate\ndisplay (show saved)",
    )
    .await;
    assert_eq!(explicit_display_output(&result)["text"], "Preparation 7");
    let declaration = displayed(&mut campaign, policy.as_ref(), "readDelivery :: Delivery -> Int\nreadDelivery (Preparation n) = n\nreadDelivery (Complete n) = n\ndisplay (readDelivery candidate)").await;
    assert_eq!(explicit_display_output(&declaration)["text"], "7");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn work_actor_consumes_later_progress_without_rearming() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/progress-route-producer.hs"),
    )
    .await;
    let (producer, _producer_binding) = next_project_worker(&mut campaign).await;
    committed(
        root.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/progress-route.hs"),
    )
    .await;
    committed(
        producer.policy.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/progress-route-questions.hs"),
    )
    .await;
    for (questions, expected, effects) in [
        ("[first]", "[[\"question-a\"]]", "1"),
        ("[first]", "[[\"question-a\"]]", "1"),
        ("[first,second]", "[[\"question-a\",\"question-b\"]]", "2"),
    ] {
        committed(
            producer.policy.as_ref(),
            &format!("import Tidepool.Agent.Reply (pollReply)\nreportProgress (WorkProgress [] {questions})\npollReply sessionReply"),
        )
        .await;
        let observed = displayed(&mut campaign, root.as_ref(), "import qualified Prelude as Haskell\nview <- readWork forwarding\n_ <- display (Haskell.show (map (map questionKey . workQuestions . sourceProgress) (collectedWork view)))\n_ <- Actor.call wakes (RoutingCount 0 id) >>= display").await;
        assert_eq!(
            explicit_display_texts(&observed),
            [expected, effects],
            "{observed}"
        );
    }
    let replied =
        dispatch_haskell_script(producer.policy.as_ref(), "respond (\"finished\" :: Text)").await;
    assert_eq!(replied["status"], "replied", "{replied}");
    let closed = displayed(&mut campaign, root.as_ref(), "view <- readWork forwarding\n_ <- display (show (map Exomonad.Contrib.Routing.sourceStatus (collectedWork view)))\n_ <- finishWork forwarding >>= display . show").await;
    let pages = explicit_display_texts(&closed);
    assert_eq!(pages[0], "[WorkClosed]", "{closed}");
    assert!(pages[1].starts_with("Completed"), "{closed}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

async fn workspace_campaign() -> TestCampaign {
    workspace_campaign_with(|_| {}).await
}

async fn workspace_campaign_with(configure: impl FnOnce(&Path)) -> TestCampaign {
    TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            crate::exomonad::workspace::copy_authored(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exomonad/examples/workspace"),
                &config.workspace,
            )
            .unwrap();
            configure(&authored);
            super::test_campaign::commit_workspace(&config.workspace);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await
}

#[tokio::test]
async fn usage_comparisons_deduplicate_resumes_and_preserve_unknown_intervals() {
    let mut campaign = workspace_campaign().await;
    let policy = campaign.root_installation.policy.clone();
    let result = displayed(
        &mut campaign,
        policy.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/usage_comparisons.hs",
        ),
    )
    .await;
    let output = explicit_display_output(&result)["text"].as_str().unwrap();
    assert!(output.contains("True"), "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn workspace_recipe_modules_and_snapshot_helpers_compile() {
    let mut campaign = workspace_campaign().await;
    let policy = campaign.root_installation.policy.clone();
    let result = displayed(
        &mut campaign,
        policy.as_ref(),
        "observed <- snapshot\ndisplay (inspectFull (swarmUsage observed))",
    )
    .await;
    assert!(
        explicit_display_output(&result)["text"]
            .as_str()
            .unwrap()
            .contains("unknownActors = [("),
        "{result}"
    );
    // Authored Display instances render workspace records with the harness's
    // own model-facing forms nested inside; Show keeps the constructor dump.
    let candidate = displayed(
        &mut campaign,
        policy.as_ref(),
        "display (inspectFull (Candidate (GitOid \"3f2a9c\") [\"cargo test\"] []))",
    )
    .await;
    let candidate = explicit_display_output(&candidate)["text"]
        .as_str()
        .unwrap();
    assert!(candidate.starts_with("Candidate {"), "{candidate}");
    assert!(
        candidate.contains("candidateCommit = 3f2a9c"),
        "{candidate}"
    );
    assert!(!candidate.contains("GitOid"), "{candidate}");
    let lookup = dispatch_lookup(
        policy.as_ref(),
        &[
            "implement",
            "reviewCandidate",
            "requestReview",
            "repair",
            "withDecision",
            "unfoldWork",
            "lunaLead",
        ],
    )
    .await;
    assert_eq!(lookup["status"], "committed", "{lookup}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

async fn next_project_worker(
    campaign: &mut TestCampaign,
) -> (
    exomonad_actor::LocalResidentInstallation,
    Arc<dyn exomonad_actor::ForkWorkspaceCustody>,
) {
    let (installation, custody, _) = next_project_activation(campaign).await;
    (installation, custody)
}

async fn next_project_activation(
    campaign: &mut TestCampaign,
) -> (
    exomonad_actor::LocalResidentInstallation,
    Arc<dyn exomonad_actor::ForkWorkspaceCustody>,
    exomonad_actor::ResidentActivation,
) {
    enum WorkerArrival {
        Child(Box<exomonad_actor::LocalResidentInstallation>),
        Ready(exomonad_actor::ResidentActivation),
    }
    let mut worker: Option<Box<exomonad_actor::LocalResidentInstallation>> = None;
    loop {
        let arrival = campaign
            .next_deployment(
                "worker admission/activation",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(child) => {
                        Ok(WorkerArrival::Child(child))
                    }
                    LocalResidentDeployment::SessionReady { activation }
                        if worker.as_ref().is_some_and(|child| {
                            child.actor.identity() == activation.id.actor()
                        }) =>
                    {
                        Ok(WorkerArrival::Ready(activation))
                    }
                    LocalResidentDeployment::WatchChanged { notification } => {
                        if let exomonad_actor::WatchTransition::RouteFailed { detail } =
                            &notification.transition
                        {
                            panic!("route failed while awaiting worker admission: {detail}");
                        }
                        Err(LocalResidentDeployment::WatchChanged { notification })
                    }
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        panic!(
                            "actor {actor:?} retired while awaiting worker admission: {terminal:?}"
                        );
                    }
                    other => Err(other),
                },
            )
            .await;
        match arrival {
            WorkerArrival::Child(child) => worker = Some(child),
            WorkerArrival::Ready(activation) => {
                let child = worker.take().unwrap();
                let binding = open_test_fork(campaign, &child);
                return (*child, binding, activation);
            }
        }
    }
}

#[tokio::test]
async fn independent_admission_rejects_inheritance_and_supervised_escape() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    let fixture = tidepool_testing::fixture_source(
        "bridge/facade/src/actor_host/fixtures/independent_worker_setup.hs",
    );
    let inherited = fixture
        .replace("withContext (selected id)", "withContext inherited")
        .replace("peer <- unfold", "peerAttempt <- attemptUnfold");
    committed(root.as_ref(), &inherited).await;
    let result = displayed(
        &mut campaign,
        root.as_ref(),
        "display (either show (const \"unexpected acceptance\") peerAttempt)",
    )
    .await;
    assert!(
        result.to_string().contains("UnfoldUncapturedContext"),
        "{result}"
    );
    committed(root.as_ref(), &fixture.replace("SwarmOwned", "ActorOwned")).await;
    let (worker, _binding) = next_project_worker(&mut campaign).await;
    assert_eq!(
        worker.supervisor_parent,
        Some(campaign.root_installation.actor.identity())
    );
    let attempted = fixture
        .replace("projectHead", "currentCheckout")
        .replace("peer <- unfold", "standaloneAttempt <- attemptUnfold");
    committed(worker.policy.as_ref(), &attempted).await;
    let result = displayed(
        &mut campaign, worker.policy.as_ref(),
        "_ <- display (either show (const \"unexpected acceptance\") standaloneAttempt)\n_ <- pollReply sessionReply >>= display . show",
    )
    .await;
    assert!(
        explicit_display_texts(&result)[0].contains("only a top-level actor"),
        "{result}"
    );
    assert_eq!(explicit_display_texts(&result)[1], "ReplyOpen", "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn independent_workers_retain_peer_requests_after_creator_retirement() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/independent_worker_setup.hs",
        ),
    )
    .await;
    let (worker, _worker_binding) = next_project_worker(&mut campaign).await;
    assert!(worker.supervisor_parent.is_none());
    assert!(worker.context_parent.is_none());
    assert_eq!(
        worker.creator,
        Some(campaign.root_installation.actor.identity())
    );
    let result =
        dispatch_haskell_script(worker.policy.as_ref(), "respond (\"ready\" :: Text)").await;
    assert_eq!(result["status"], "replied", "{result}");
    committed(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/independent_peer_setup.hs",
        ),
    )
    .await;
    let (observer, _observer_binding) = next_project_worker(&mut campaign).await;
    assert!(observer.supervisor_parent.is_none());
    assert_eq!(observer.creator, worker.creator);
    let unshared = displayed(&mut campaign, observer.policy.as_ref(), "let retainedPeer = sessionInput\nvisibleBefore <- snapshot\n_ <- display (length (snapshotActors visibleBefore))\n_ <- shareObservation retainedPeer retainedPeer >>= display . show").await;
    assert_eq!(explicit_display_texts(&unshared)[0], "1", "{unshared}");
    assert_eq!(
        explicit_display_texts(&unshared)[1],
        "ObservationUnauthorized",
        "{unshared}"
    );
    assert_eq!(
        campaign
            .forest
            .inspect_graph(observer.actor.identity())
            .unwrap()
            .len(),
        1
    );
    let status = dispatch_status(observer.policy.as_ref(), "lineage").await;
    assert!(
        !status["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains(&worker.label),
        "{status}"
    );
    let shared = displayed(
        &mut campaign, root.as_ref(),
        "_ <- shareObservation (responseActor peerObserver) (responseActor peer) >>= display . show",
    )
    .await;
    assert_eq!(
        explicit_display_output(&shared)["text"],
        "ObservationShared",
        "{shared}"
    );
    let observed = displayed(&mut campaign, observer.policy.as_ref(), "visibleAfter <- snapshot\n_ <- display (length (snapshotActors visibleAfter))\n_ <- stopAgent retainedPeer >>= display . show").await;
    assert_eq!(explicit_display_texts(&observed)[0], "2", "{observed}");
    assert_eq!(
        explicit_display_texts(&observed)[1],
        "StopUnauthorized",
        "{observed}"
    );
    assert_eq!(
        campaign
            .forest
            .inspect_graph(observer.actor.identity())
            .unwrap()
            .len(),
        2
    );
    let status = dispatch_status(observer.policy.as_ref(), "lineage").await;
    assert!(
        status["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains(&worker.label),
        "{status}"
    );
    let result =
        dispatch_haskell_script(observer.policy.as_ref(), "respond (\"ready\" :: Text)").await;
    assert_eq!(result["status"], "replied", "{result}");
    campaign
        .root_installation
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "planner finished".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    assert!(worker.actor.terminal().get().is_none());
    assert!(observer.actor.terminal().get().is_none());
    committed(observer.policy.as_ref(), "let followupLabel = [label|peer-followup|]\nfollowup <- do { issued <- request @Text retainedPeer (assignment followupLabel (\"after planner retirement\" :: Text)); Right () <- detachRequest issued; pure issued }").await;
    let worker_actor = worker.actor.identity();
    campaign
        .next_deployment(
            "peer-followup request activation",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == worker_actor =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let result = dispatch_haskell_script(
        worker.policy.as_ref(),
        "respond (sessionInput <> \" accepted\" :: Text)",
    )
    .await;
    assert_eq!(result["status"], "replied", "{result}");
    let result = displayed(
        &mut campaign,
        observer.policy.as_ref(),
        "settled <- pollResponse followup\ndisplay (show settled)",
    )
    .await;
    assert!(
        explicit_display_output(&result)["text"]
            .as_str()
            .unwrap()
            .contains("after planner retirement accepted"),
        "{result}"
    );
    worker
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "peer finished".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    let stale = displayed(
        &mut campaign,
        observer.policy.as_ref(),
        "_ <- shareObservation retainedPeer retainedPeer >>= display . show",
    )
    .await;
    assert_eq!(
        explicit_display_output(&stale)["text"],
        "ObservationRecipientUnavailable",
        "{stale}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    assert_eq!(
        worker.actor.terminal().get().unwrap().kind,
        ActorExitKind::Completed
    );
    assert_eq!(
        observer.actor.terminal().get().unwrap().kind,
        ActorExitKind::Cancelled
    );
    assert!(campaign
        .forest
        .new_workbench(
            "after-swarm-stop".into(),
            exomonad_actor::EffectiveRole::root()
        )
        .await
        .is_err());
}

#[tokio::test]
async fn project_review_retains_evidence_and_owns_direct_repair() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    let source = campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    displayed(
        &mut campaign,
        root.as_ref(),
        &format!("let sourceHead = GitOid \"{}\"", source.as_str()),
    )
    .await;
    displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_delivery_setup.hs"),
    )
    .await;
    let (implementer, _implementer_binding) = next_project_worker(&mut campaign).await;
    assert_eq!(
        implementer.instructions.as_deref(),
        Some(include_str!(
            "../../../../exomonad/examples/workspace/.exomonad/prompts/task.md"
        ))
    );
    let tree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &implementer.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    let candidate = campaign
        ._repository
        .writer_at(tree.cwd())
        .commit_file("feature.txt", "candidate\n", "implement feature")
        .unwrap();
    let replied = dispatch_haskell_script(
        implementer.policy.as_ref(),
        &format!(
            "respond (Produced (Candidate (GitOid \"{}\") [\"focused candidate check\"] [\"open product gate\"]))",
            candidate.as_str()
        ),
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_review_start.hs"),
    )
    .await;
    let (reviewer, _reviewer_binding) = next_project_worker(&mut campaign).await;
    let review_source = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &reviewer.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        exomonad_worktree::git::GitCli::new()
            .run(review_source.cwd(), &["rev-parse", "HEAD"])
            .unwrap()
            .trimmed(),
        candidate.as_str(),
        "independent review must inspect the originally submitted commit"
    );
    let review_instructions =
        include_str!("../../../../exomonad/examples/workspace/.exomonad/prompts/review.md");
    assert_eq!(reviewer.instructions.as_deref(), Some(review_instructions));
    let launched = super::developer_instructions_selected(
        &reviewer.effective_role,
        None,
        reviewer.instructions.as_deref(),
    );
    assert!(launched.starts_with(review_instructions));
    assert!(launched.contains("Runtime policy ("));
    let evidence = displayed(&mut campaign, reviewer.policy.as_ref(), "_ <- display . show $ (reviewInput sessionInput)\nlet RetainedImplementer repairTarget = repairOwner sessionInput\n_ <- display . show $ (agentIdentity repairTarget)").await;
    assert!(
        explicit_item_display_text(&evidence, 0).contains("focused candidate check"),
        "{evidence}"
    );
    assert!(
        explicit_item_display_text(&evidence, 0).contains("reportedChecks"),
        "authored check summaries remain claims: {evidence}"
    );
    assert!(
        explicit_item_display_text(&evidence, 0).contains("open product gate"),
        "{evidence}"
    );
    assert_eq!(
        explicit_item_display_text(&evidence, 2)
            .split_whitespace()
            .collect::<String>(),
        format!(
            "({},{})",
            implementer.actor.identity().id.0,
            implementer.actor.identity().incarnation.0
        )
    );
    displayed(
        &mut campaign,
        reviewer.policy.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_review_repair.hs"),
    )
    .await;
    let pending = displayed(
        &mut campaign,
        reviewer.policy.as_ref(),
        "import Tidepool.Agent.Reply (pollReply)\n_ <- pollReply sessionReply >>= display . show",
    )
    .await;
    assert_eq!(
        explicit_item_display_text(&pending, 1),
        "ReplyOpen",
        "{pending}"
    );
    let implementer_actor = implementer.actor.identity();
    campaign
        .next_deployment(
            "repair request activation",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == implementer_actor =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let repair_packet = displayed(&mut campaign,
        implementer.policy.as_ref(),
        "_ <- display . show $ (taskSource (repairAssignment sessionInput), repairInput sessionInput, repairFindings sessionInput)",
    ).await;
    for expected in [
        source.as_str(),
        candidate.as_str(),
        "open product gate",
        "preserve the product gate",
    ] {
        assert!(
            repair_packet.to_string().contains(expected),
            "missing {expected} from typed repair packet: {repair_packet}"
        );
    }
    let repair = displayed(
        &mut campaign,
        implementer.policy.as_ref(),
        "_ <- display (show sessionInput)",
    )
    .await;
    assert!(
        explicit_item_display_text(&repair, 0).contains("preserve the product gate"),
        "{repair}"
    );
    let revised = campaign
        ._repository
        .writer_at(tree.cwd())
        .commit_file("feature.txt", "repaired\n", "repair feature")
        .unwrap();
    let replied = dispatch_haskell_script(
        implementer.policy.as_ref(),
        &format!(
            "respond (Produced (Candidate (GitOid \"{}\") [\"focused repair check\"] [\"open product gate\"]))",
            revised.as_str()
        ),
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    let result = displayed(&mut campaign, reviewer.policy.as_ref(), "state <- pollWatch repaired\n_ <- display . show $ (fmap (either (const False) (const True) . settledValue) state)").await;
    assert_eq!(
        explicit_item_display_text(&result, 1),
        "WatchReady True",
        "{result}"
    );
    displayed(
        &mut campaign,
        reviewer.policy.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_design_question.hs"),
    )
    .await;
    let (expert, _expert_binding) = next_project_worker(&mut campaign).await;
    assert_eq!(expert.model.as_deref(), Some("planner"));
    assert_eq!(expert.fork_effort, Some(exomonad_actor::ForkEffort::Medium));
    assert_eq!(expert.supervisor_parent, Some(reviewer.actor.identity()));
    assert_eq!(expert.context_parent, None);
    let question = displayed(
        &mut campaign,
        expert.policy.as_ref(),
        "_ <- display (show sessionInput)",
    )
    .await;
    for expected in [
        revised.as_str(),
        "focused repair check",
        "feature review",
        "retain the gate",
    ] {
        assert!(
            question.to_string().contains(expected),
            "missing {expected}: {question}"
        );
    }
    let expert_tree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &expert.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    let amendment = campaign
        ._repository
        .writer_at(expert_tree.cwd())
        .commit_file(
            "plans/feature.md",
            "Preparation retains the open product gate.\n",
            "clarify acceptance",
        )
        .unwrap();
    let answered = dispatch_haskell_script(
        expert.policy.as_ref(),
        &format!("respond (AmendPlan (PlanAmendment (GitOid \"{}\") (GitOid \"{}\") [\"plans/feature.md\"] \"retain the preparation gate\" [\"feature review\"] [\"boundary evidence\"]))", revised.as_str(), amendment.as_str()),
    ).await;
    assert_eq!(answered["status"], "replied", "{answered}");
    let decision = displayed(&mut campaign, reviewer.policy.as_ref(), "design <- pollWatch designReady\n_ <- display . show $ (fmap settledValue design)\n_ <- pollReply sessionReply >>= display . show").await;
    assert!(
        explicit_item_display_text(&decision, 1).contains("retain the preparation gate"),
        "{decision}"
    );
    assert_eq!(
        explicit_item_display_text(&decision, 2),
        "ReplyOpen",
        "{decision}"
    );
    displayed(
        &mut campaign,
        reviewer.policy.as_ref(),
        &tidepool_testing::fixture_source(
            ".exomonad/workspace/checks/project_plan_incorporation.hs",
        ),
    )
    .await;
    let implementer_actor = implementer.actor.identity();
    campaign
        .next_deployment(
            "incorporation request activation",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == implementer_actor =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let offered = displayed(
        &mut campaign,
        implementer.policy.as_ref(),
        "_ <- display . show $ (incorporationAmendment sessionInput)",
    )
    .await;
    assert!(
        offered.to_string().contains(amendment.as_str()),
        "{offered}"
    );
    let git = exomonad_worktree::git::GitCli::new();
    git.run(tree.cwd(), &["merge", "--ff-only", amendment.as_str()])
        .unwrap();
    let incorporated_head = git.run(tree.cwd(), &["rev-parse", "HEAD"]).unwrap();
    assert_eq!(
        std::fs::read_to_string(tree.cwd().join("plans/feature.md")).unwrap(),
        "Preparation retains the open product gate.\n"
    );
    let incorporated = dispatch_haskell_script(implementer.policy.as_ref(),
        &format!("respond (Incorporated (incorporationAmendment sessionInput) (GitOid \"{}\") [\"read exact plan at resulting head\"])", incorporated_head.trimmed())).await;
    assert_eq!(incorporated["status"], "replied", "{incorporated}");
    let checked = displayed(&mut campaign, reviewer.policy.as_ref(), "incorporation <- pollWatch planReady\n_ <- display . show $ (fmap settledValue incorporation)\n_ <- pollReply sessionReply >>= display . show").await;
    for expected in [
        "Incorporated",
        incorporated_head.trimmed(),
        "read exact plan at resulting head",
    ] {
        assert!(
            explicit_item_display_text(&checked, 1).contains(expected),
            "{checked}"
        );
    }
    assert_eq!(
        explicit_item_display_text(&checked, 2),
        "ReplyOpen",
        "{checked}"
    );
    let questions = displayed(
        &mut campaign,
        reviewer.policy.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_review_questions.hs")
            .replace("inspectFull ", "_ <- display . show $ ")
            .replace(
                "pollReply sessionReply",
                "_ <- pollReply sessionReply >>= display . show",
            )
            .replace(
                "pollResponse reviewer",
                "_ <- pollResponse reviewer >>= display . show",
            ),
    )
    .await;
    assert_eq!(
        explicit_display_texts(&questions).last().copied().unwrap(),
        "ReplyOpen",
        "{questions}"
    );
    displayed(
        &mut campaign,
        root.as_ref(),
        &format!(
            "let incorporatedHead = GitOid \"{}\"",
            incorporated_head.trimmed()
        ),
    )
    .await;
    let pending = displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_decision_return.hs")
            .replace("inspectFull ", "_ <- display . show $ ")
            .replace(
                "pollReply sessionReply",
                "_ <- pollReply sessionReply >>= display . show",
            )
            .replace(
                "pollResponse reviewer",
                "_ <- pollResponse reviewer >>= display . show",
            ),
    )
    .await;
    assert!(pending.to_string().contains("ResponsePending"), "{pending}");
    // Carries the producing actor's own progress, so this poll answers "is
    // it moving" without a second round trip.
    assert!(
        pending.to_string().contains("pendingActorState"),
        "{pending}"
    );
    let delivery = campaign
        .next_deployment(
            "decision request update",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::RequestUpdate { delivery } => Ok(delivery),
                LocalResidentDeployment::SessionReady { activation } => {
                    panic!("decision queued a new obligation: {:?}", activation.id)
                }
                other => Err(other),
            },
        )
        .await;
    let presentation = delivery.begin().unwrap();
    assert!(presentation
        .message()
        .contains("Preparation retains the boundary"));
    assert!(presentation.message().contains(incorporated_head.trimmed()));
    presentation.presented();
    let status = displayed(
        &mut campaign,
        root.as_ref(),
        "_ <- pollRequestUpdate clarification >>= display . show",
    )
    .await;
    assert_eq!(
        explicit_item_display_text(&status, 0),
        "Right UpdatePresented",
        "{status}"
    );
    // Source incorporation remains distinct from presenting the accepted decision.
    let review_tree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &reviewer.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    git.run(
        review_tree.cwd(),
        &["merge", "--ff-only", incorporated_head.trimmed()],
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(review_tree.cwd().join("plans/feature.md")).unwrap(),
        "Preparation retains the open product gate.\n"
    );
    let propagated = displayed(
        &mut campaign,
        reviewer.policy.as_ref(),
        &tidepool_testing::fixture_source(
            ".exomonad/workspace/checks/project_decision_consumer.hs",
        )
        .replace("inspectFull ", "_ <- display . show $ ")
        .replace(
            "pollReply sessionReply",
            "_ <- pollReply sessionReply >>= display . show",
        )
        .replace(
            "pollResponse reviewer",
            "_ <- pollResponse reviewer >>= display . show",
        ),
    )
    .await;
    assert!(
        propagated.to_string().contains("(True,True,True,True)"),
        "{propagated}"
    );
    assert_eq!(
        explicit_display_texts(&propagated).last().copied().unwrap(),
        "ReplyOpen",
        "{propagated}"
    );
    let (consumer, _consumer_binding, activation) = next_project_activation(&mut campaign).await;
    assert_eq!(consumer.context_parent, None);
    let selected = &activation.message;
    for expected in [
        incorporated_head.trimmed(),
        "Preparation retains the boundary",
        "read exact plan at resulting head",
        "Why:",
        "Preserve the product gate",
    ] {
        assert!(
            selected.contains(expected),
            "missing {expected} from fresh context: {selected}"
        );
    }
    let consumer_context = displayed(
        &mut campaign,
        consumer.policy.as_ref(),
        "_ <- display . show $ (taskContext sessionInput)",
    )
    .await;
    assert!(
        consumer_context
            .to_string()
            .contains(incorporated_head.trimmed()),
        "{consumer_context}"
    );
    let consumer_tree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &consumer.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        git.run(consumer_tree.cwd(), &["rev-parse", "HEAD"])
            .unwrap()
            .trimmed(),
        incorporated_head.trimmed()
    );
    let attention = displayed(
        &mut campaign,
        root.as_ref(),
        "remaining <- pollProgress reviewQuestions\n_ <- display (show remaining)",
    )
    .await;
    assert!(
        attention.to_string().contains("product-gate"),
        "{attention}"
    );
    assert!(
        !explicit_item_display_text(&attention, 1).contains("questionKey = \"semantics\""),
        "answered question survived: {attention}"
    );
    let original = displayed(
        &mut campaign,
        root.as_ref(),
        "original <- pollResponse worker\n_ <- display (show original)",
    )
    .await;
    assert!(
        explicit_item_display_text(&original, 1).contains(candidate.as_str()),
        "{original}"
    );
    assert!(
        !explicit_item_display_text(&original, 1).contains(revised.as_str()),
        "repair changed the original response: {original}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn route_finishes_owned_request_without_a_model_relay() {
    route_reply_case(false).await;
}

#[tokio::test]
async fn route_reply_preserves_request_cancellation() {
    route_reply_case(true).await;
}

async fn route_reply_case(cancel: bool) {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    let source = campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    displayed(
        &mut campaign,
        root.as_ref(),
        "let routeCampaign = \"route-reply\" :: CampaignLabel",
    )
    .await;
    displayed(
        &mut campaign,
        root.as_ref(),
        &format!("let sourceHead = GitOid \"{}\"", source.as_str()),
    )
    .await;
    displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/route-reply-setup.hs"),
    )
    .await;
    let (lead, _lead_binding) = next_project_worker(&mut campaign).await;
    displayed(
        &mut campaign,
        lead.policy.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/route-reply-worker.hs"),
    )
    .await;
    let (worker, _worker_binding) = next_project_worker(&mut campaign).await;
    if cancel {
        let result = displayed(
            &mut campaign,
            root.as_ref(),
            "_ <- cancelRequest lead >>= display . show",
        )
        .await;
        assert!(
            result.to_string().contains("CancellationRequested"),
            "{result}"
        );
    }
    let replied = dispatch_haskell_script(
        worker.policy.as_ref(),
        "respond (Candidate (GitOid \"exact-candidate\") [\"checked\"] [\"open gate\"])",
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    // Observe the requester on success: the lead never needs a relay turn.
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = if cancel {
                displayed(
                    &mut campaign,
                    lead.policy.as_ref(),
                    "_ <- pollRoute forwarding >>= display . show",
                )
                .await
            } else {
                displayed(
                    &mut campaign,
                    root.as_ref(),
                    "answer <- pollResponse lead\n_ <- display $ inspectFull answer",
                )
                .await
            };
            let output = result.to_string();
            if output.contains("exact-candidate") || output.contains("RouteFailed") {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    if cancel {
        assert!(
            outcome.to_string().contains("CancellationRequested"),
            "{outcome}"
        );
        let pending = displayed(
            &mut campaign, lead.policy.as_ref(),
            "import Tidepool.Agent.Reply (pollReply)\n_ <- pollReply sessionReply >>= display . show",
        )
        .await;
        assert!(
            pending.to_string().contains("ReplyCancellationRequested"),
            "{pending}"
        );
    } else {
        let response = outcome;
        let route = displayed(
            &mut campaign,
            lead.policy.as_ref(),
            "_ <- pollRoute forwarding >>= display . show",
        )
        .await;
        assert!(route.to_string().contains("RouteCompleted"), "{route}");
        assert!(
            response.to_string().contains("exact-candidate"),
            "{response}"
        );
        assert!(response.to_string().contains("open gate"), "{response}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn frozen_prompt_bytes_round_trip_through_haskell() {
    let mut campaign = workspace_campaign_with(|authored| {
        crate::exomonad::edit_fixture_project_config(authored, |project| {
            project
                .prompts
                .files
                .insert("literal".into(), "prompts/literal.md".into());
        });
        std::fs::write(
            authored.join("prompts/literal.md"),
            "\u{1}f\0".to_owned() + "9\n\"\\\tλ\u{7f}",
        )
        .unwrap();
    })
    .await;
    let policy = campaign.root_installation.policy.clone();
    let result = displayed(
        &mut campaign,
        policy.as_ref(),
        "display (show (fmap (map fromEnum . T.unpack) (workspacePrompt \"literal\")))",
    )
    .await;
    // Explicit Show preserves the exact code points, including control bytes.
    let output = explicit_display_output(&result)["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{result}"));
    assert_eq!(
        output.replace('\n', ""),
        "Just [1,102,0,57,10,34,92,9,955,127]",
        "{result}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

fn recipe_workspace(checks: Option<&[&str]>) -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    // A candidate is a project, and a project is a Git tree: that is how `nix`
    // reads the `flake.nix` a package's pinned Haskell source is named in.
    let git = exomonad_worktree::GitCli::new();
    git.init_repository(repository.path(), &["--quiet"])
        .unwrap();
    git.try_run(
        repository.path(),
        &["config", "user.name", "Exomonad recipe check"],
    )
    .unwrap();
    git.try_run(
        repository.path(),
        &["config", "user.email", "recipe-check@localhost"],
    )
    .unwrap();
    crate::exomonad::workspace::copy_authored(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exomonad/examples/workspace"),
        repository.path(),
    )
    .unwrap();
    if let Some(checks) = checks {
        crate::exomonad::edit_fixture_project_config(
            &repository.path().join(".exomonad"),
            |project| {
                project.haskell.checks = checks.iter().map(|entry| (*entry).into()).collect();
            },
        );
    }
    super::test_campaign::commit_workspace(repository.path());
    repository
}

/// `exomonad check --workspace` asks GHC whether every public child profile
/// can install the selected spec and retains the profile, entry, and compiler
/// evidence when one cannot.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_check_refuses_a_spec_no_child_role_can_satisfy() {
    let repository = recipe_workspace(None);
    let spec = repository.path().join(".exomonad/AgentSpec.hs");
    let original = std::fs::read_to_string(&spec).unwrap();
    let widened = original
        .replacen(
            "import Tidepool.Effects.Core (BoundWorktree, Commands, Jev, Lookup, Reflect)",
            "import Tidepool.Effects.Core (BoundWorktree, Commands, Jev, Lookup, Reflect)\nimport Tidepool.Effects (Journal)",
            1,
        )
        .replacen(
            "{-# LANGUAGE FlexibleContexts #-}",
            "{-# LANGUAGE FlexibleContexts #-}\n{-# LANGUAGE ConstraintKinds #-}",
            1,
        )
        .replacen(
            "Member BoundWorktree effects\n  ) =>",
            "Member BoundWorktree effects, RequiresJournal effects\n  ) =>",
            1,
        )
        .replacen(
            "agentSpec ::",
            "type RequiresJournal effects = Member Journal effects\n\nagentSpec ::",
            1,
        );
    assert_ne!(
        widened, original,
        "fixture's AgentSpec.hs no longer matches either replaced string"
    );
    std::fs::write(&spec, widened).unwrap();
    super::test_campaign::commit_workspace(repository.path());
    let error = crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("profile research cannot install spec AgentSpec.agentSpec"),
        "{message}"
    );
    assert!(
        message.contains("profile coding cannot install spec AgentSpec.agentSpec"),
        "{message}"
    );
    assert!(
        message.contains("Journal") && message.contains("<agent-spec-installation>"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_check_refuses_sleep_required_by_a_spec_under_wider_role_ceilings() {
    let repository = recipe_workspace(None);
    crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .expect("the original spec installs under every concrete public profile");
    std::fs::write(
        repository.path().join(".exomonad/AgentSpec.hs"),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/sleep_required_spec.hs",
        ),
    )
    .unwrap();
    super::test_campaign::commit_workspace(repository.path());
    let error = crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .unwrap_err();
    let message = error.to_string();
    for profile in exomonad_tool::PublicActorProfile::ALL {
        assert!(
            message.contains(&format!(
                "profile {} cannot install spec AgentSpec.agentSpec",
                profile.label()
            )),
            "{message}"
        );
    }
    assert!(
        message.contains("Sleep") && message.contains("<agent-spec-installation>"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_check_ignores_constraints_on_a_later_helper() {
    let repository = recipe_workspace(None);
    let spec = repository.path().join(".exomonad/AgentSpec.hs");
    std::fs::write(
        &spec,
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/unconstrained_spec_later_helper.hs",
        ),
    )
    .unwrap();
    super::test_campaign::commit_workspace(repository.path());
    crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn candidate_workspace_runs_its_own_model_free_recipes() {
    let repository = recipe_workspace(None);
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn recipe_checks_reject_a_candidate_only_defect_and_accept_its_repair() {
    let repository = recipe_workspace(Some(&["Project.CollaborationChecks.collaboration"]));
    let work = repository
        .path()
        .join(".exomonad/checks/project_decision_consumer.hs");
    let original = std::fs::read_to_string(&work).unwrap();
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
    let broken = original.replacen(
        "resolveQuestion acceptedDecision changedQuestions == changedQuestions",
        "resolveQuestion acceptedDecision changedQuestions /= changedQuestions",
        1,
    );
    assert_ne!(broken, original, "fixture no longer matches replaced text");
    std::fs::write(&work, broken).unwrap();
    // This candidate fixture is data, not a compiled module; it still compiles,
    // and the running selected worker must expose its defect.
    crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .unwrap();
    let error = crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("an old answer cannot clear a changed question or rewind the task source"),
        "{error}"
    );
    std::fs::write(&work, original).unwrap();
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

/// A recipe module that fails to COMPILE (not merely fails an assertion) must
/// still surface GHC's own diagnostic text — file:line:col, severity, and
/// message — not just `CompileError`'s one-line "N diagnostic(s)" summary.
/// Regression for the dev-friction bug where a lane had to reconnect to the
/// compile daemon by hand to see what GHC actually said.
#[tokio::test(flavor = "multi_thread")]
async fn recipe_check_compile_failure_reports_the_ghc_diagnostic_text() {
    let repository = recipe_workspace(Some(&["Project.CollaborationChecks.collaboration"]));
    let work = repository.path().join(".exomonad/Project/Checks.hs");
    let original = std::fs::read_to_string(&work).unwrap();
    let broken = original.replace(
        "readFile actor (checkSource name) >>= void . turn actor",
        "readFile actor (checkSource name) >>= void . undefinedRecipeIdentifierXyz",
    );
    assert_ne!(broken, original, "fixture no longer matches replaced text");
    std::fs::write(&work, broken).unwrap();
    super::test_campaign::commit_workspace(repository.path());
    let error = crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("Haskell compilation failed") || message.contains("compilation failed"),
        "{message}"
    );
    assert!(
        message.contains("undefinedRecipeIdentifierXyz") && message.contains("not in scope"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn candidate_routing_recipes_exercise_failure_and_attention() {
    let repository = recipe_workspace(Some(&["Project.RoutingChecks.routing"]));
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_handoff_recipe_integrates_later_final_heads_from_both_lanes() {
    let repository = recipe_workspace(Some(&["Project.RoutingChecks.twoLaneHandoff"]));
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn attention_actor_recipe_retains_independent_sources_through_closure() {
    let repository = recipe_workspace(Some(&["Project.RoutingChecks.independentSources"]));
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

#[tokio::test]
async fn work_router_queries_receipts_as_the_issuing_actor() {
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let package = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .unwrap()
                .join("exomonad/examples/workspace");
            crate::exomonad::workspace::copy_authored(&package, &config.workspace).unwrap();
            super::test_campaign::commit_workspace(&config.workspace);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await;
    let root = campaign.root_installation.policy.clone();
    displayed(
        &mut campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/work_notification.hs"),
    )
    .await;
    let source = campaign
        .next_deployment(
            "the progress source",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(source) => Ok(source),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "the source request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let publisher = source.policy.clone();
    let publication = tokio::spawn(async move {
        committed(publisher.as_ref(),
            "reportProgress (WorkProgress [] [Question \"decision\" (DesignQuestion \"plans/test.md\" (GitOid \"candidate\") \"choose the boundary\" [] [] [])])"
        ).await
    });
    let message = campaign
        .next_deployment(
            "the router's native message",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationSend(message) => Ok(message),
                other => Err(other),
            },
        )
        .await;
    let sender = message.owner();
    assert_ne!(sender, campaign.actor.identity());
    assert_eq!(message.target(), campaign.actor.identity());
    let directory = tempfile::tempdir().unwrap();
    let inbox = ActorInbox::open(
        &tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path()).unwrap(),
        "rows",
        "cursor",
    )
    .unwrap();
    let key = "work-router-inbox";
    admit_notification(&message, key.into(), &inbox);
    publication.await.unwrap();
    let wrong_owner = displayed(&mut campaign, root.as_ref(),
        "view <- readWork collector\nlet [receipt] = [r | Notice _ (Right r) <- workNotices view]\n_ <- pollNotification receipt >>= display . show"
    ).await;
    assert!(
        wrong_owner.to_string().contains("NotificationUnauthorized"),
        "{wrong_owner}"
    );
    let policy = root.clone();
    let query = tokio::spawn(async move {
        committed(
            policy.as_ref(),
            "_ <- R.call (workNotification (R.client collector)) receipt >>= display . show",
        )
        .await
    });
    let poll = campaign
        .next_deployment(
            "a receipt query without another message",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationPoll(poll) => Ok(poll),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(poll.owner(), sender);
    let observed = observe_notification_receipt(&poll, campaign.actor.identity(), key, &inbox);
    assert_eq!(observed, Ok(exomonad_actor::NotificationState::Accepted));
    poll.observed(observed);
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let result = campaign.drive_actor_output(&store, query).await.unwrap();
    assert!(
        result.to_string().contains("NotificationAccepted"),
        "{result}"
    );
    let replaced = displayed(&mut campaign, root.as_ref(),
        "collector <- R.replace collector (workDefinition sources (notifyWork owner (workMessage id)))\n_ <- R.call (workNotification (R.client collector)) receipt >>= display . show"
    ).await;
    assert!(
        replaced.to_string().contains("NotificationUnauthorized"),
        "{replaced}"
    );
    let retained = displayed(
        &mut campaign,
        root.as_ref(),
        "_ <- readWork collector >>= display . length . workNotices",
    )
    .await;
    assert_eq!(
        explicit_display_output(&retained)["text"],
        "1",
        "{retained}"
    );
    committed(root.as_ref(), "finishWork collector").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

fn explicit_item_display_text(reply: &serde_json::Value, item: usize) -> &str {
    reply["items"][item]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|operation| operation.get("display"))
        .unwrap_or_else(|| panic!("structured display missing from item {item}: {reply}"))["text"]
        .as_str()
        .unwrap()
}

fn explicit_display_texts(reply: &serde_json::Value) -> Vec<&str> {
    reply["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["operations"].as_array().unwrap())
        .filter_map(|operation| operation.get("display"))
        .map(|display| display["text"].as_str().unwrap())
        .collect()
}

fn explicit_display_output(reply: &serde_json::Value) -> &serde_json::Value {
    reply["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item.get("operations"))
        .flat_map(|operations| {
            operations
                .as_array()
                .unwrap_or_else(|| panic!("structured operations are not an array: {reply}"))
        })
        .find_map(|operation| operation.get("display"))
        .unwrap_or_else(|| panic!("structured display metadata missing: {reply}"))
}

fn explicit_display_identity(display: &serde_json::Value) -> (i64, i64, i64) {
    let identity = display["identity"].as_array().unwrap();
    (
        identity[0].as_i64().unwrap(),
        identity[1].as_i64().unwrap(),
        identity[2].as_i64().unwrap(),
    )
}

#[tokio::test]
async fn explicit_display_expands_siblings_without_compilation_or_repeated_effects() {
    use super::command_test_support::backend_request;
    use super::command_test_support::TestCommands;

    let mut campaign = TestCampaign::start().await;
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let policy = campaign.root_installation.policy.clone();
    let invocation = ToolInvocationContext::external(
        "display-qualification".into(),
        "expansion-cancellation".into(),
        "expansion-cancellation".into(),
        Some("expansion-cancellation".into()),
        None,
    );
    let invoking_policy = policy.clone();
    let dispatch_context = invocation.clone();
    let mut running = tokio::spawn(async move {
        invoking_policy
            .dispatch_boxed(ToolInvocation {
                context: Some(dispatch_context),
                name: exomonad_actor::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/notebook_explicit_display_siblings.hs",
                )),
            })
            .await
    });
    let backend = TestCommands::completed(&"x".repeat(4000));
    tokio::select! {
        request = backend_request(&mut campaign) => request.supply(Ok(backend.clone())),
        result = &mut running => panic!("display ended before its authored command: {result:?}"),
    }
    let initial = campaign
        .next_deployment(
            "initial display publication",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::DisplayPublished(request) => Ok(request),
                other => Err(other),
            },
        )
        .await;
    let run = super::runtime_namespace(campaign.session_root.path());
    super::display_output::publish(&campaign.forest, &store, &run, None, None, &initial);
    let display = serde_json::to_value(&initial.page).unwrap();
    let identity = explicit_display_identity(&display);
    let origin = harness::store::actor_output::ActorOutputOrigin {
        run: super::runtime_namespace(campaign.session_root.path()),
        native_actor: identity.0 as u64,
        incarnation: identity.1 as u64,
    };
    let initial_history = store.actor_output_page(&origin, 0, 10).unwrap();
    assert_eq!(initial_history.outputs.len(), 1);
    assert!(initial_history.outputs[0].emission().conversation.is_none());
    assert_eq!(
        initial.outcome(),
        Some(exomonad_actor::DisplayPublicationOutcome::Published(
            tidepool_runtime::session::ActorOutputReference {
                run: run.clone(),
                sequence: initial_history.outputs[0].reference().sequence,
            }
        ))
    );
    let keys = display["expansions"].as_array().unwrap();
    assert_eq!(
        keys.len(),
        2,
        "both fields are independently addressable: {display}"
    );
    let left = keys[0][0].as_i64().unwrap();
    let right = keys[1][0].as_i64().unwrap();
    assert!(display["text"].as_str().unwrap().contains("DisplayPair"));

    let pending = campaign
        .next_deployment(
            "authored expansion publication before host acknowledgement",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::DisplayPublished(request) => Ok(request),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(pending.page.identity, identity);
    assert_eq!(pending.page_ordinal, 2);
    assert!(
        pending.operation.is_some(),
        "authored expansion has an execution owner"
    );
    let (committed, commit_observed) = tokio::sync::oneshot::channel();
    let (release_ack, ack_released) = std::sync::mpsc::channel();
    let held_host = tokio::task::spawn_blocking({
        let forest = campaign.forest.clone();
        let store = store.clone();
        let run = run.clone();
        let pending = pending.clone();
        move || {
            super::display_output::publish_before_ack(&forest, &store, &run, &pending, || {
                committed.send(()).unwrap();
                ack_released.recv_timeout(Duration::from_secs(60)).unwrap();
            })
        }
    });
    tokio::time::timeout(Duration::from_secs(30), commit_observed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .actor_output_page(&origin, 0, 10)
            .unwrap()
            .outputs
            .len(),
        2
    );
    assert!(pending.outcome().is_none());
    let frozen_context = pending.host_context().unwrap().clone();
    let committed_page = store.actor_output_page(&origin, 0, 10).unwrap();
    let committed_sequence = committed_page.outputs[1].reference().sequence;
    let committed_emission = committed_page.outputs[1].emission().clone();

    // Source admission is complete. Only native retry/expansion may run below.
    tidepool_extract_cmd::reset_extract_spawn_count();
    let cancelled = tokio::time::timeout(
        Duration::from_secs(30),
        policy.cancel_workbench_boxed(invocation.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    let exomonad_actor::WorkbenchCancellationOutcome::Cancelled {
        reply: delivered, ..
    } = cancelled
    else {
        panic!("expected the actual notebook cancellation owner: {cancelled:?}")
    };
    let receipts: &[tidepool_runtime::session::WorkbenchItemReceipt] = match &delivered {
        Ok(response) => &response.items,
        Err(failure) => failure.receipts(),
    };
    let uncertain = receipts
        .iter()
        .flat_map(|item| &item.operations)
        .find(|operation| operation.id == *pending.operation.as_ref().unwrap())
        .unwrap();
    use tidepool_runtime::session::WorkbenchDisplayPublication;
    let publication = match uncertain.display_publication.as_ref().unwrap() {
        WorkbenchDisplayPublication::Pending { publication } => publication,
        WorkbenchDisplayPublication::Unconfirmed {
            publication,
            detail,
        } => {
            assert!(detail.len() <= 2048);
            publication
        }
        other => panic!("first delivered reply must preserve uncertainty: {other:?}"),
    };
    assert_eq!(publication.display, identity);
    assert_eq!(publication.page_ordinal, 2);
    assert!(uncertain.display.is_none());
    let frozen_bytes = serde_json::to_vec(receipts).unwrap();
    let original = tokio::time::timeout(Duration::from_secs(30), running)
        .await
        .unwrap()
        .unwrap();
    match original {
        Err(exomonad_actor::ResidentToolError::Invocation(failure)) => {
            assert_eq!(delivered, Err(failure))
        }
        Ok(output) => {
            let response = delivered
                .as_ref()
                .expect("a successful tool reply must carry the cancelled native response");
            assert_eq!(
                response.status,
                tidepool_runtime::session::WorkbenchRunStatus::RequestCancelled
            );
            match output {
                exomonad_actor::ResidentToolResponse::Workbench(output) => {
                    assert_eq!(&output, response);
                }
                other => panic!("cancelled notebook lost its typed workbench receipt: {other:?}"),
            }
        }
        other => {
            panic!("original tool transport lost the canonical cancellation receipt: {other:?}")
        }
    }
    assert!(pending.was_unconfirmed());

    let retried_policy = policy.clone();
    let retried =
        tokio::spawn(async move { retried_policy.expand_display_boxed(identity, left).await });
    let resubmitted = campaign
        .next_deployment(
            "native retry of committed output whose acknowledgement was lost",
            Duration::from_secs(30),
            |event| match event {
                LocalResidentDeployment::DisplayPublished(request) => Ok(request),
                other => Err(other),
            },
        )
        .await;
    assert!(Arc::ptr_eq(&resubmitted, &pending));
    assert_eq!(resubmitted.page_ordinal, 2);
    assert_eq!(resubmitted.host_context(), Some(&frozen_context));
    super::display_output::publish(&campaign.forest, &store, &run, None, None, &resubmitted);
    let expanded = tokio::time::timeout(Duration::from_secs(30), retried)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    release_ack.send(()).unwrap();
    held_host.await.unwrap();
    let replay = policy.cancel_workbench_boxed(invocation).await.unwrap();
    let exomonad_actor::WorkbenchCancellationOutcome::Cancelled { reply: replay, .. } = replay
    else {
        panic!("cancellation replay lost its original outcome: {replay:?}")
    };
    assert_eq!(replay, delivered);
    let replay_receipts: &[tidepool_runtime::session::WorkbenchItemReceipt] = match &replay {
        Ok(response) => &response.items,
        Err(failure) => failure.receipts(),
    };
    assert_eq!(serde_json::to_vec(replay_receipts).unwrap(), frozen_bytes);
    assert_eq!(
        store
            .actor_output_page(&origin, 0, 10)
            .unwrap()
            .outputs
            .len(),
        2
    );
    let reconciled = store.actor_output_page(&origin, 0, 10).unwrap();
    assert_eq!(
        reconciled.outputs[1].reference().sequence,
        committed_sequence
    );
    assert_eq!(reconciled.outputs[1].emission(), &committed_emission);
    assert_eq!(expanded["status"], "committed", "{expanded}");
    let next = explicit_display_output(&expanded);
    assert_eq!(explicit_display_identity(next), identity);
    assert_eq!(next["expansions"][0][0], right, "sibling key stays stable");
    assert_eq!(next["expansions"].as_array().unwrap().len(), 1);
    assert!(policy.expand_display_boxed(identity, left).await.is_err());
    assert!(policy
        .expand_display_boxed((identity.0, identity.1 + 1, identity.2), right)
        .await
        .is_err());
    let final_page = campaign
        .drive_actor_output(&store, policy.expand_display_boxed(identity, right))
        .await
        .unwrap();
    assert!(explicit_display_output(&final_page)["expansions"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        0,
        "native expansion must not invoke the source compiler"
    );
    assert_eq!(backend.executions(), 1, "authored effects execute once");
    let history = store.actor_output_page(&origin, 0, 10).unwrap();
    assert_eq!(
        history.outputs.len(),
        3,
        "one durable row per displayed page"
    );
    assert_eq!(
        history
            .outputs
            .iter()
            .map(|output| output.emission().id.page_ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    assert!(policy.expand_display_boxed(identity, right).await.is_err());
    assert_eq!(
        store
            .actor_output_page(&origin, 0, 10)
            .unwrap()
            .outputs
            .len(),
        3
    );
    assert!(
        display["text"].as_str().unwrap().contains("DisplayPair"),
        "historical output stays readable after retirement"
    );
}

#[tokio::test]
async fn explicit_display_rejects_callback_effect_before_authority_input() {
    let mut campaign = TestCampaign::start().await;
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let policy = campaign.root_installation.policy.clone();
    let published = campaign
        .drive_actor_output(
            &store,
            committed(
                policy.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/notebook_explicit_display_untrusted_callback.hs",
                ),
            ),
        )
        .await;
    let identity = explicit_display_identity(explicit_display_output(&published));
    tidepool_extract_cmd::reset_extract_spawn_count();
    assert!(policy.expand_display_boxed(identity, 1).await.is_err());
    assert_eq!(tidepool_extract_cmd::extract_spawn_count(), 0);
    assert_eq!(
        explicit_display_output(&published)["text"],
        "forged preview"
    );
    let origin = harness::store::actor_output::ActorOutputOrigin {
        run: super::runtime_namespace(campaign.session_root.path()),
        native_actor: identity.0 as u64,
        incarnation: identity.1 as u64,
    };
    assert_eq!(
        store
            .actor_output_page(&origin, 0, 10)
            .unwrap()
            .outputs
            .len(),
        1
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn explicit_display_respects_shared_budget_and_handles_survive_cell_failure() {
    let mut campaign = TestCampaign::start().await;
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let policy = campaign.root_installation.policy.clone();
    let failed = campaign
        .drive_actor_output(
            &store,
            super::test_campaign::dispatch_haskell_script_result(
                policy.as_ref(),
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/notebook_explicit_display_budget_failure.hs",
                ),
            ),
        )
        .await;
    let Err(exomonad_actor::ResidentToolError::Invocation(
        exomonad_actor::KernelInvocationFailure::Workbench(failure),
    )) = failed
    else {
        panic!("authored display failure must preserve native failure receipts: {failed:?}")
    };
    let receipts = serde_json::to_value(&failure.receipts).unwrap();
    let displays: Vec<_> = receipts
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["operations"].as_array().unwrap())
        .filter_map(|operation| operation.get("display"))
        .collect();
    assert_eq!(
        displays.len(),
        2,
        "both publications survive the later failure"
    );
    assert_eq!(displays[1]["text"], "second");
    let first = displays[0];
    let prefix = first["text"].as_str().unwrap();
    assert!(
        prefix.len() < 5000,
        "the prior console output consumes this page's budget"
    );
    let identity = explicit_display_identity(first);
    let keys = first["expansions"].as_array().unwrap();
    assert_eq!(keys.len(), 1, "the unrendered prefix remains addressable");
    let key = keys[0][0].as_i64().unwrap();
    tidepool_extract_cmd::reset_extract_spawn_count();
    let expanded = campaign
        .drive_actor_output(&store, policy.expand_display_boxed(identity, key))
        .await
        .unwrap();
    let suffix = explicit_display_output(&expanded)["text"].as_str().unwrap();
    assert_eq!(format!("{prefix}{suffix}"), "v".repeat(5000));
    assert_eq!(tidepool_extract_cmd::extract_spawn_count(), 0);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
