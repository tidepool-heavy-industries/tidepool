//! Focused execution of the published examples through the real resident tool.

use super::test_campaign::TestCampaign;
use super::test_campaign::{
    dispatch_haskell_script, dispatch_haskell_script_result, dispatch_lookup, dispatch_status,
};
use super::*;
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

/// Authored notebook sources used by both the published fences and resident tests.
#[derive(Clone, Copy)]
enum PublishedExample {
    HumanForms,
    Request,
    Command,
    Lookup,
    Reflect,
}

impl PublishedExample {
    fn identity(self) -> &'static str {
        match self {
            Self::HumanForms => "human-forms",
            Self::Request => "request",
            Self::Command => "command",
            Self::Lookup => "lookup",
            Self::Reflect => "reflect",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::HumanForms => include_str!("fixtures/api-guide/human-forms.hs"),
            Self::Request => include_str!("fixtures/api-guide/request.hs"),
            Self::Command => include_str!("fixtures/api-guide/command.hs"),
            Self::Lookup => include_str!("fixtures/api-guide/lookup.hs"),
            Self::Reflect => include_str!("fixtures/api-guide/reflect.hs"),
        }
    }
}

fn assert_published_sources(document: &str, expected: &[PublishedExample]) {
    let mut published = std::collections::BTreeMap::new();
    for fence in document.split("```").skip(1).step_by(2) {
        let (info, source) = fence.split_once('\n').expect("fence has a body");
        if info == "haskell signatures" {
            continue;
        }
        if info.starts_with("haskell") {
            let identity = info
                .strip_prefix("haskell source=")
                .expect("executable Haskell fence needs an authored source identity");
            assert!(
                published.insert(identity, source).is_none(),
                "duplicate source {identity}"
            );
        }
    }
    for example in expected {
        assert_eq!(
            published.remove(example.identity()),
            Some(example.source()),
            "published source {} differs from its compiled fixture",
            example.identity()
        );
    }
    assert!(
        published.is_empty(),
        "unknown executable sources: {published:?}"
    );
}

#[test]
fn published_notebook_sources_match_authored_fixtures() {
    assert_published_sources(
        include_str!("../../../../exomonad/prompts/api-guide.md"),
        &[
            PublishedExample::HumanForms,
            PublishedExample::Request,
            PublishedExample::Command,
            PublishedExample::Lookup,
        ],
    );
    assert_published_sources(
        include_str!("../../../../exomonad/prompts/docs/reflect.md"),
        &[PublishedExample::Reflect],
    );
}

#[tokio::test]
async fn captured_child_keeps_original_nominal_types_after_parent_shadowing_and_failure() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        include_str!("fixtures/captured_context_original_owner_setup.hs"),
    )
    .await;

    let child = campaign
        .next_deployment(
            "captured child installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    let child_id = child.actor.identity();
    campaign.assert_no_deployment("captured child remains idle", |event| {
        matches!(event, LocalResidentDeployment::SessionReady { activation } if activation.id.actor() == child_id)
    });

    committed(
        root.as_ref(),
        "Right originalOwnerRequest <- request @OriginalReply originalOwner (OriginalInput 41) defaultRequestOptions",
    )
    .await;
    campaign
        .next_deployment(
            "captured child typed request activation",
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

    committed(
        root.as_ref(),
        "data OriginalInput = LaterInput Bool deriving Show\ndata OriginalReply = LaterReply Bool deriving Show",
    )
    .await;
    let parent_failure = dispatch_haskell_script_result(
        root.as_ref(),
        "error \"parent-failure-after-captured-request\" >> pure ()",
    )
    .await
    .expect_err("the parent cell deliberately fails after shadowing its types");
    let parent_failure = format!("{parent_failure:?}");
    assert!(
        parent_failure.contains("parent-failure-after-captured-request"),
        "{parent_failure}"
    );
    assert!(campaign.actor.terminal().get().is_none());

    let reply = dispatch_haskell_script(
        child.policy.as_ref(),
        "case sessionInput of OriginalInput n -> respond (OriginalReply (n + 1))",
    )
    .await;
    assert_eq!(reply["status"], "replied", "{reply}");
    let observed = displayed(
        campaign,
        root.as_ref(),
        "originalOwnerReply <- pollResponse originalOwnerRequest\ndisplay (show originalOwnerReply)",
    )
    .await;
    let text = explicit_display_output(&observed)["text"].as_str().unwrap();
    assert!(text.contains("OriginalReply 42"), "{observed}");
    assert!(!text.contains("LaterReply"), "{observed}");
})).await;
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
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.as_ref();
                for source in [":status", ":{\ncolonOnly = 1\n:}"] {
                    let result = dispatch_haskell_script(policy, source).await;
                    assert_eq!(result["status"], "rejected", "source={source}: {result}");
                }
            })
        })
        .await;
}

#[tokio::test]
async fn explicit_display_preserves_resource_guidance_and_specialized_renderers() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
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
            })
        })
        .await;
}

#[tokio::test]
async fn explicit_display_is_the_only_value_presentation() {
    use super::command_test_support::{backend_request, TestCommands};

    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
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
        request = backend_request(campaign) => request.supply(Ok(backend.clone())),
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
})).await;
}

#[tokio::test]
async fn notebook_cell_relocates_same_cell_types_and_rejects_before_installation() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
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
            })
        })
        .await;
}

#[tokio::test]
async fn notebook_cell_prologue_applies_to_check_stage_and_execution() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
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
            })
        })
        .await;
}

#[tokio::test]
async fn notebook_cell_rejection_retains_its_source_plan() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
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
            })
        })
        .await;
}

#[tokio::test]
async fn notebook_cell_preserves_old_types() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
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
            })
        })
        .await;
}

#[tokio::test]
async fn notebook_cell_retains_observations_needed_by_its_suffix() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
                let root = campaign.root_installation.policy.clone();
                let initial = committed(root.as_ref(), "(42 :: Int)\n").await;
                let saved = initial["items"][0]["installedBindings"][0]
                    .as_str()
                    .unwrap();
                let source = tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/notebook_lease_suffix.hs",
                )
                .replace("__SAVED__", saved);
                let result = campaign
                    .drive_actor_output(&store, committed(root.as_ref(), &source))
                    .await;
                assert_eq!(explicit_display_output(&result)["text"], "42", "{result}");
            })
        })
        .await;
}

#[tokio::test]
async fn notebook_cell_infers_response_results() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let response = displayed(
                    campaign,
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/notebook_identity_response.hs",
                    ),
                )
                .await;
                let output = explicit_display_output(&response)["text"].as_str().unwrap();
                assert!(output.contains("Pending"), "{response:?}");
                committed(root.as_ref(), "later\npollResponse pending\n").await;
            })
        })
        .await;
}

#[tokio::test]
async fn notebook_cell_reply_marks_its_tail_not_run() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                committed(
                    root.as_ref(),
                    concat!(
            "import qualified Tidepool.Agent.Contract as A\n",
            "Right worker <- spawnSubagent (FreshCtx \"notebook reply worker\") SameDir ",
            "(defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))\n",
        ),
                )
                .await;
                committed(
        root.as_ref(),
        "Right response <- request @Text worker (\"ready\" :: Text) defaultRequestOptions\n",
    )
    .await;
                let child = campaign
                    .next_deployment(
                        "notebook reply child policy installation",
                        Duration::from_secs(120),
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
                        "notebook reply request activation",
                        Duration::from_secs(120),
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

                let reply = dispatch_haskell_script(
                    child.policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/notebook_reply_tail.hs",
                    ),
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
            })
        })
        .await;
}

fn open_test_workspace(
    campaign: &TestCampaign,
    child: &exomonad_actor::LocalResidentInstallation,
) -> Arc<dyn exomonad_actor::WorkspaceCustody> {
    let [worktree_id] = child.launch_worktrees.as_slice() else {
        panic!("child must have one worktree")
    };
    let worktree_id = exomonad_worktree::WorktreeId::from_raw(worktree_id);
    let worktree = campaign.worktrees.lookup(&worktree_id).unwrap().unwrap();
    assert!(worktree.cwd().is_dir(), "installation workspace must exist");
    let principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(campaign.session_root.path()),
        child.actor.identity().id.0,
        child.actor.identity().incarnation.0,
    );
    assert!(
        campaign
            .bindings
            .lock()
            .membership(&worktree_id, &principal)
            .is_some(),
        "installation must retain exact workspace membership"
    );
    let binding = child
        .worktree_custody
        .clone()
        .expect("bootstrap installed custody");
    binding
}

#[tokio::test]
async fn record_actor_launch_publishes_and_routes_child_reply() {
    let campaign = TestCampaign::start_with_config(
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
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                committed(
                    root.as_ref(),
                    r#"import LaunchFixture
import Tidepool.Worktree (workspaceFor)
Right launcherTree <- createWorktree (fromRef (GitRef "HEAD") "resident-launcher")
Right launcherWorkspace <- workspaceFor launcherTree
launcher <- R.start (R.withWorkspace launcherWorkspace launchDefinition)"#,
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
                let _custody = open_test_workspace(campaign, &child);
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
                        arguments: ToolArguments::Raw(
                            "respond (\"review complete\" :: Text)".into(),
                        ),
                    })
                    .await
                    .unwrap();
                assert_eq!(reply["status"], "replied", "{reply}");
                let result = displayed(
                    campaign,
                    root.as_ref(),
                    "R.call (readReply (R.client launcher)) () >>= display",
                )
                .await;
                assert!(result.to_string().contains("review complete"), "{result}");
            })
        })
        .await;
}

#[tokio::test]
async fn published_request_example_retains_success_and_target_cancellation() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        include_str!("fixtures/api-guide/request-setup.hs"),
    )
    .await;
    let mut running = tokio::spawn({
        let root = root.clone();
        async move { committed(root.as_ref(), PublishedExample::Request.source()).await }
    });
    let child = tokio::select! {
        child = campaign.next_deployment(
            "published request child installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        ) => child,
        result = &mut running => panic!("request example ended before child installation: {result:?}"),
    };
    let _binding = open_test_workspace(campaign, &child);
    campaign
        .next_deployment(
            "published typed request activates its child",
            Duration::from_secs(120),
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
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let completed = campaign.drive_actor_output(&store, running).await.unwrap();
    assert_eq!(completed["status"], "committed", "{completed}");
    let success = displayed(campaign, root.as_ref(), "display (answer == input)").await;
    assert_eq!(
        explicit_display_output(&success)["text"],
        "True",
        "{success}"
    );

    committed(
        root.as_ref(),
        "Right interrupted <- request @Text worker input defaultRequestOptions",
    )
    .await;
    campaign
        .next_deployment(
            "second typed request activates its child",
            Duration::from_secs(120),
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
    let failure = displayed(
        campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/shared_api_guide_unavailable.hs",
        ),
    )
    .await;
    assert_eq!(
        explicit_display_output(&failure)["text"],
        "(True, True, True)",
        "{failure}"
    );
    campaign
        .next_deployment(
            "published request child retirement",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::Retired { actor, .. }
                    if actor == child.actor.identity() =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
})).await;
}

#[tokio::test]
async fn published_human_form_example_handles_unbound_host() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        Arc::new(harness::store::Store::open(directory.path().join("store.sqlite")).unwrap());
    let host = Arc::new(DisplayOnlyHost {
        display: super::form_output::host(
            store.clone(),
            "published-human-form".into(),
            Arc::new(std::sync::OnceLock::new()),
        ),
        openings: std::sync::atomic::AtomicUsize::new(0),
    });
    let campaign = TestCampaign::start_with_form_host(host.clone()).await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    displayed(
        campaign,
        root.as_ref(),
        PublishedExample::HumanForms.source(),
    )
    .await;
    let unavailable = displayed(
        campaign,
        root.as_ref(),
        "display (case scopeAnswer of { F.FormUnavailable F.FormNotInstalled -> True; _ -> False })",
    )
    .await;
    assert_eq!(
        explicit_display_output(&unavailable)["text"],
        "True",
        "{unavailable}"
    );
    assert_eq!(host.openings.load(std::sync::atomic::Ordering::SeqCst), 1);
    let origin = harness::store::actor_output::ActorOutputOrigin {
        run: "published-human-form".into(),
        native_actor: campaign.actor.identity().id.0,
        incarnation: campaign.actor.identity().incarnation.0,
    };
    let pages = store.actor_output_page(&origin, 0, 10).unwrap();
    assert_eq!(
        pages.outputs.len(),
        3,
        "narration, evidence, unavailable branch"
    );
    assert!(pages
        .outputs
        .iter()
        .all(|output| output.emission().page.view.is_some()));
})).await;
}

/// Rich views and form admission share FormHost. Delegate views to the real
/// durable owner while declining form admission with its typed absent-service cause.
struct DisplayOnlyHost {
    display: Arc<dyn exomonad_actor::FormHost>,
    openings: std::sync::atomic::AtomicUsize,
}

impl exomonad_actor::FormHost for DisplayOnlyHost {
    fn changed(
        &self,
    ) -> futures_util::future::BoxFuture<'static, Result<(), tidepool_bridge_effects::FormCause>>
    {
        panic!("a refused form must not wait for submission")
    }

    fn open(
        &self,
        _: &exomonad_actor::FormPublication,
        _: &str,
        _: &serde_json::Value,
    ) -> Result<(), tidepool_bridge_effects::FormCause> {
        self.openings
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(tidepool_bridge_effects::FormCause::FormNotInstalled)
    }

    fn attempt(
        &self,
        _: exomonad_actor::ActorRef,
        _: &str,
    ) -> Result<Option<tidepool_bridge_effects::FormAttempt>, tidepool_bridge_effects::FormCause>
    {
        panic!("a refused form must not read a submission")
    }

    fn reject(
        &self,
        _: exomonad_actor::ActorRef,
        _: &str,
        _: &str,
        _: &serde_json::Value,
    ) -> Result<tidepool_bridge_effects::FormTransition, tidepool_bridge_effects::FormCause> {
        panic!("a refused form must not reject a submission")
    }

    fn commit(
        &self,
        _: exomonad_actor::ActorRef,
        _: &str,
        _: &str,
        _: &serde_json::Value,
    ) -> Result<tidepool_bridge_effects::FormTransition, tidepool_bridge_effects::FormCause> {
        panic!("a refused form must not commit a submission")
    }

    fn close(
        &self,
        _: exomonad_actor::ActorRef,
        _: &str,
    ) -> Result<(), tidepool_bridge_effects::FormCause> {
        panic!("refused admission must not retain a form lease for cleanup")
    }

    fn display(
        &self,
        publication: &exomonad_actor::FormPublication,
        display_slot: u64,
        view: &serde_json::Value,
    ) -> Result<(), tidepool_bridge_effects::FormCause> {
        self.display.display(publication, display_slot, view)
    }
}

#[tokio::test]
async fn published_command_example_preserves_failed_output_without_replay() {
    use super::command_test_support::TestCommands;

    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let mut running = tokio::spawn({
                    let root = root.clone();
                    async move { committed(root.as_ref(), PublishedExample::Command.source()).await }
                });
                let backend = TestCommands::completed("README.md");
                tokio::select! {
                    request = campaign.next_deployment(
                        "published command backend",
                        Duration::from_secs(180),
                        |event| match event {
                            LocalResidentDeployment::CommandBackend(request) => Ok(request),
                            other => Err(other),
                        },
                    ) => request.supply(Ok(backend.clone())),
                    result = &mut running => panic!("command example ended before backend admission: {result:?}"),
                }
                let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
                let completed = campaign.drive_actor_output(&store, running).await.unwrap();
                assert_eq!(completed["status"], "committed", "{completed}");
                assert!(explicit_display_output(&completed)["text"]
                    .as_str()
                    .unwrap()
                    .contains("README.md"), "{completed}");
                for _ in 0..2 {
                    let retained = displayed(
                        campaign,
                        root.as_ref(),
                        "display (Cmd.stdout result == Right \"README.md\")",
                    )
                    .await;
                    assert_eq!(
                        explicit_display_output(&retained)["text"],
                        "True",
                        "{retained}"
                    );
                }
                assert_eq!(
                    backend.executions(),
                    1,
                    "reading stdout must not repeat the command"
                );

                *backend.stdout.lock() = "/workspace\nREADME.md\n".into();
                *backend.stderr.lock() = "bash: sed: command not found\n".into();
                backend.set_exit_code(127);
                let failed = displayed(campaign, root.as_ref(), PublishedExample::Command.source()).await;
                let observed = explicit_display_output(&failed)["text"].as_str().unwrap();
                assert!(observed.contains("127"), "failed exit code missing: {failed}");
                for stream in ["/workspace\nREADME.md\n", "bash: sed: command not found\n"] {
                    assert!(observed.contains(stream), "stream missing from command observation: {failed}");
                }
                for _ in 0..2 {
                    let retained = displayed(
                        campaign,
                        root.as_ref(),
                        "display (Cmd.commandOutcome (Cmd.commandResult result) == Cmd.CommandExited 127 && Cmd.commandCleanup (Cmd.commandResult result) == Cmd.CommandClean && Cmd.stdout result == Left (Cmd.Unsuccessful (Cmd.CommandExited 127)) && Cmd.stderr result == Right \"bash: sed: command not found\\n\")",
                    ).await;
                    assert_eq!(explicit_display_output(&retained)["text"], "True", "{retained}");
                }
                assert_eq!(backend.executions(), 2, "observing failure must not replay either command");
            })
        })
        .await;
}

#[tokio::test]
async fn published_lookup_and_reflect_examples_preserve_boundary_results() {
    let campaign = TestCampaign::start_with_conversation(
        |admission| admission,
        |_| {},
        Some(Arc::new(|_, _| {
            Box::pin(async { Err(exomonad_actor::ConversationUnavailable::Unbound) })
        })),
    )
    .await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                displayed(campaign, root.as_ref(), PublishedExample::Lookup.source()).await;
                let found = displayed(
                    campaign,
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/lookup_topics_found.hs",
                    ),
                )
                .await;
                assert_eq!(explicit_display_output(&found)["text"], "True", "{found}");
                let reflected =
                    displayed(campaign, root.as_ref(), PublishedExample::Reflect.source()).await;
                assert_eq!(
                    explicit_display_output(&reflected)["text"],
                    "[]",
                    "{reflected}"
                );
                let unbound = displayed(
        campaign,
        root.as_ref(),
        "seen <- reflect 5\ndisplay (case seen of { Left ReflectUnbound -> True; _ -> False })",
    )
    .await;
                assert_eq!(
                    explicit_display_output(&unbound)["text"],
                    "True",
                    "{unbound}"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn activation_presents_prose_and_preserves_exact_inputs() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let mut child = None;
                committed(
                    root.as_ref(),
                    concat!(
            "import qualified Tidepool.Agent.Contract as A\n",
            "data Report = Report Int deriving Show\n",
            "Right worker <- spawnSubagent (FreshCtx \"activation preview worker\") SameDir ",
            "(defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))",
        ),
                )
                .await;
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
                    committed(
                        root.as_ref(),
                        &format!(
                "Right previewResponse <- request @Report worker {input} defaultRequestOptions"
            ),
                    )
                    .await;
                    let activation = campaign
                        .next_deployment("preview activation", Duration::from_secs(120), |event| {
                            match event {
                                LocalResidentDeployment::PolicyInstalled(installation) => {
                                    child = Some((*installation).clone());
                                    Err(LocalResidentDeployment::PolicyInstalled(installation))
                                }
                                LocalResidentDeployment::SessionReady { activation }
                                    if child.as_ref().is_some_and(|child| {
                                        activation.id.actor() == child.actor.identity()
                                    }) =>
                                {
                                    Ok(activation)
                                }
                                other => Err(other),
                            }
                        })
                        .await;
                    assert!(activation
                        .message
                        .contains("`reportProgress` is unavailable"));
                    if label == "preview-text" {
                        let unavailable = dispatch_lookup(
                            child.as_ref().unwrap().policy.as_ref(),
                            &["reportProgress"],
                        )
                        .await;
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
                            campaign,
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
                    let result =
                        dispatch_haskell_script(child.as_ref().unwrap().policy.as_ref(), reply)
                            .await;
                    assert_eq!(result["status"], "replied", "{result:?}");
                }
                committed(root.as_ref(), "stopAgent worker").await;
            })
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rich_response_survives_resident_computation() {
    // The published review recipe computes typed decisions and verifies that
    // their scope, task and candidate values survive the retained response.
    let repository = recipe_workspace(Some(&["Project.SkillChecks.reviewProvenance"]));
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

#[tokio::test]
async fn quiet_observation_retains_exact_results_without_repeating_effects() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
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
                            LocalResidentDeployment::PolicyInstalled(installation) => {
                                Ok(installation)
                            }
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
                let reply =
                    dispatch_haskell_script(child.policy.as_ref(), "respond delivery").await;
                assert_eq!(reply["status"], "replied", "{reply}");
                campaign.await_watch_ready().await;
                let first = committed(root.as_ref(), "pollWatch ready").await;
                let saved = first["items"][0]["installedBindings"][0].as_str().unwrap();
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
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
                let expired =
                    dispatch_haskell_script(root.as_ref(), &format!("{expiring} ()")).await;
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
                    concat!(
                        "import qualified Tidepool.Agent.Contract as A\n",
                        "Right spawned <- spawnSubagent (FreshCtx \"observe once\") SameDir ",
                        "(defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies]))",
                    ),
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
            })
        })
        .await;
}

#[tokio::test]
async fn model_selection_is_independent_of_inherited_and_selected_context() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                committed(
                    root.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/fixtures/model_context.hs",
                    ),
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
                                    LocalResidentDeployment::SessionReady { .. } => {
                                        Ok(Arrival::Ready)
                                    }
                                    other => Err(other),
                                },
                            )
                            .await;
                        match arrival {
                            Arrival::Child(child) => {
                                bindings.push(open_test_workspace(campaign, &child));
                                children.push(child);
                            }
                            Arrival::Ready => ready += 1,
                        }
                    }
                }
                for child in &children {
                    assert_eq!(child.model.as_deref(), Some("gpt-6-sol"));
                    assert_eq!(child.supervisor_parent, Some(campaign.actor.identity()));
                    if child.label == "exact" {
                        assert_eq!(child.context_parent, Some(campaign.actor.identity()));
                        let inherited = displayed(
                            campaign,
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
                    let reply =
                        dispatch_haskell_script(child.policy.as_ref(), "respond sessionInput")
                            .await;
                    assert_eq!(reply["status"], "replied", "{reply}");
                }
            })
        })
        .await;
}

#[tokio::test]
async fn routes_forward_without_model_relay_and_retain_callback_failure() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    displayed(
        campaign,
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
                    bindings.push(open_test_workspace(campaign, &child));
                    children.push(child);
                }
                Arrival::Ready => ready += 1,
            }
        }
    }
    let consumer = children
        .iter()
        .find(|child| child.label == "consumer")
        .unwrap();
    let producer = children
        .iter()
        .find(|child| child.label == "producer")
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
                    bindings.push(open_test_workspace(campaign, &child));
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
        campaign,
        root.as_ref(),
        "_ <- pollRoute forwarding >>= display . show\n_ <- pollRoute broken >>= display . show",
    )
    .await;
    assert_eq!(explicit_display_texts(&state)[0], "RouteCompleted");
    assert!(
        explicit_display_texts(&state)[1].contains("deliberate route failure"),
        "{state}"
    );
    let recovered = displayed(campaign, root.as_ref(), "recovered <- listRoutes\n_ <- display $ inspectFull (length recovered)\nstates <- traverse pollRoute recovered\n_ <- display (show states)").await;
    assert_eq!(explicit_display_texts(&recovered)[0], "3", "{recovered}");
    assert!(
        explicit_display_texts(&recovered)[1].contains("deliberate route failure"),
        "{recovered}"
    );
    let foreign = displayed(
        campaign,
        consumer.policy.as_ref(),
        "owned <- listRoutes\n_ <- display $ inspectFull (length owned)",
    )
    .await;
    assert_eq!(explicit_display_texts(&foreign)[0], "0", "{foreign}");
    let reply = dispatch_haskell_script(consumer.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    displayed(
        campaign,
        root.as_ref(),
        "stopAgent (responseActor producer)",
    )
    .await;
    displayed(campaign, root.as_ref(), "Right unavailable <- request @Text (responseActor producer) (\"lost target\" :: Text) defaultRequestOptions\nhandled <- route (settlement unavailable) (\\settled -> case settled of { Left _ -> pure (); Right _ -> error \"unexpected success\" })").await;
    let handled = displayed(
        campaign, root.as_ref(),
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
        campaign,
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
        campaign,
        root.as_ref(),
        "retained <- listRoutes\n_ <- display $ inspectFull (length retained)",
    )
    .await;
    assert_eq!(explicit_display_texts(&retained)[0], "2", "{retained}");
})).await;
}

#[tokio::test]
async fn configured_modules_are_available_to_resident_declarations_from_frozen_sources() {
    let campaign = TestCampaign::start_with_config(
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
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let result = displayed(
        campaign,
        policy.as_ref(),
        "saved <- pure candidate\ndisplay (show saved)",
    )
    .await;
    assert_eq!(explicit_display_output(&result)["text"], "Preparation 7");
    let declaration = displayed(campaign, policy.as_ref(), "readDelivery :: Delivery -> Int\nreadDelivery (Preparation n) = n\nreadDelivery (Complete n) = n\ndisplay (readDelivery candidate)").await;
    assert_eq!(explicit_display_output(&declaration)["text"], "7");
})).await;
}

#[tokio::test]
async fn work_actor_consumes_later_progress_without_rearming() {
    let campaign = workspace_campaign().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
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
    let (producer, _producer_binding) = next_project_worker(campaign).await;
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
        let observed = displayed(campaign, root.as_ref(), "import qualified Prelude as Haskell\nview <- readWork forwarding\n_ <- display (Haskell.show (map (map questionKey . workQuestions . sourceProgress) (collectedWork view)))\n_ <- Actor.call wakes (RoutingCount 0 id) >>= display").await;
        assert_eq!(
            explicit_display_texts(&observed),
            [expected, effects],
            "{observed}"
        );
    }
    let replied =
        dispatch_haskell_script(producer.policy.as_ref(), "respond (\"finished\" :: Text)").await;
    assert_eq!(replied["status"], "replied", "{replied}");
    let closed = displayed(campaign, root.as_ref(), "view <- readWork forwarding\n_ <- display (show (map Exomonad.Contrib.Routing.sourceStatus (collectedWork view)))\n_ <- finishWork forwarding >>= display . show").await;
    let pages = explicit_display_texts(&closed);
    assert_eq!(pages[0], "[WorkClosed]", "{closed}");
    assert!(pages[1].starts_with("Completed"), "{closed}");
})).await;
}

async fn workspace_campaign() -> TestCampaign {
    workspace_campaign_with(|_| {}).await
}

async fn workspace_campaign_with(configure: impl FnOnce(&Path)) -> TestCampaign {
    TestCampaign::start_with_conversation(
        |admission| admission,
        |config| {
            config.jev = Some(Arc::new(super::test_campaign::FixtureJev));
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
        // The model-free fixture has no provider conversation to read.
        Some(Arc::new(|_, _| {
            Box::pin(async { Err(exomonad_actor::ConversationUnavailable::Unbound) })
        })),
    )
    .await
}

#[tokio::test]
async fn usage_comparisons_deduplicate_resumes_and_preserve_unknown_intervals() {
    let campaign = workspace_campaign().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let result = displayed(
                    campaign,
                    policy.as_ref(),
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/fixtures/usage_comparisons.hs",
                    ),
                )
                .await;
                let output = explicit_display_output(&result)["text"].as_str().unwrap();
                assert!(output.contains("True"), "{result}");
            })
        })
        .await;
}

#[tokio::test]
async fn workspace_recipe_modules_and_snapshot_helpers_compile() {
    let campaign = workspace_campaign().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let result = displayed(
                    campaign,
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
                    campaign,
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
                        "workspaceAgentSpec",
                    ],
                )
                .await;
                assert_eq!(lookup["status"], "committed", "{lookup}");
            })
        })
        .await;
}

async fn next_project_worker(
    campaign: &mut TestCampaign,
) -> (
    exomonad_actor::LocalResidentInstallation,
    Arc<dyn exomonad_actor::WorkspaceCustody>,
) {
    let (installation, custody, _) = next_project_activation(campaign).await;
    (installation, custody)
}

async fn next_project_activation(
    campaign: &mut TestCampaign,
) -> (
    exomonad_actor::LocalResidentInstallation,
    Arc<dyn exomonad_actor::WorkspaceCustody>,
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
                let binding = open_test_workspace(campaign, &child);
                return (*child, binding, activation);
            }
        }
    }
}

#[tokio::test]
async fn independent_spawn_separates_context_and_ownership_and_refuses_missing_seed() {
    let campaign = workspace_campaign().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    campaign._repository.writer().stage(".exomonad").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();

    let rejected = displayed(
        campaign,
        root.as_ref(),
        concat!(
            "rejected <- spawnSubagent (FreshCtx \"bad seed control\") ",
            "(ForkWorktree (atRef (GitRef \"refs/heads/tidepool-missing-seed\"))) ",
            "(defaultSpawnOptions workspaceAgentSpec)\n",
            "display (show rejected)",
        ),
    )
    .await;
    assert!(
        explicit_display_output(&rejected)["text"]
            .as_str()
            .unwrap()
            .contains("Left"),
        "a missing committed seed must fail typed spawn admission: {rejected}"
    );
    campaign.assert_no_deployment("rejected seed did not install an actor", |event| {
        matches!(event, LocalResidentDeployment::PolicyInstalled(_))
    });

    committed(
        root.as_ref(),
        concat!(
            "let parentOnly = 41 :: Int\n",
            "Right captured <- checkpoint \"independent fork context\"\n",
            "Right peer <- spawnSubagent (ForkCtx captured) (ForkWorktree projectHead) ",
            "((defaultSpawnOptions workspaceAgentSpec) { spawnLifetime = RunOwned, ",
            "spawnLabel = Just \"run-owned-peer\" })",
        ),
    )
    .await;
    let peer = campaign
        .next_deployment(
            "independent run-owned child admission",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(peer.supervisor_parent, None);
    assert_eq!(
        peer.context_parent,
        Some(campaign.root_installation.actor.identity())
    );
    assert_eq!(
        peer.creator,
        Some(campaign.root_installation.actor.identity())
    );
    let peer_id = peer.actor.identity();
    campaign.assert_no_deployment("idle spawn waits for a typed request", |event| {
        matches!(event, LocalResidentDeployment::SessionReady { activation } if activation.id.actor() == peer_id)
    });

    committed(
        root.as_ref(),
        "Right peerRequest <- request @Text peer (\"ready\" :: Text) defaultRequestOptions",
    )
    .await;
    campaign
        .next_deployment(
            "first typed request activates the idle child",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == peer_id =>
                {
                    Ok(activation)
                }
                other => Err(other),
            },
        )
        .await;
    let reply = dispatch_haskell_script(peer.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    let answered = displayed(
        campaign,
        root.as_ref(),
        "Right answer <- await (result peerRequest)\ndisplay answer",
    )
    .await;
    assert_eq!(
        explicit_display_output(&answered)["text"],
        "ready",
        "{answered}"
    );
})).await;
}

#[tokio::test]
async fn independent_workers_retain_peer_requests_after_creator_retirement() {
    let campaign = workspace_campaign().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
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
    let (worker, _worker_binding) = next_project_worker(campaign).await;
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
    let (observer, _observer_binding) = next_project_worker(campaign).await;
    assert!(observer.supervisor_parent.is_none());
    assert_eq!(observer.creator, worker.creator);
    let unshared = displayed(campaign, observer.policy.as_ref(), "let retainedPeer = sessionInput\nvisibleBefore <- snapshot\n_ <- display (length (snapshotActors visibleBefore))\n_ <- shareObservation retainedPeer retainedPeer >>= display . show").await;
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
        campaign, root.as_ref(),
        "_ <- shareObservation (responseActor peerObserver) (responseActor peer) >>= display . show",
    )
    .await;
    assert_eq!(
        explicit_display_output(&shared)["text"],
        "ObservationShared",
        "{shared}"
    );
    let observed = displayed(campaign, observer.policy.as_ref(), "visibleAfter <- snapshot\n_ <- display (length (snapshotActors visibleAfter))\n_ <- stopAgent retainedPeer >>= display . show").await;
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
    committed(observer.policy.as_ref(), "Right followup <- request @Text retainedPeer (\"after planner retirement\" :: Text) defaultRequestOptions").await;
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
        campaign,
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
        campaign,
        observer.policy.as_ref(),
        "_ <- shareObservation retainedPeer retainedPeer >>= display . show",
    )
    .await;
    assert_eq!(
        explicit_display_output(&stale)["text"],
        "ObservationRecipientUnavailable",
        "{stale}"
    );
    campaign.observe_shutdown().await.unwrap();
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
            exomonad_actor::ActorCapabilities::default()
        )
        .await
        .is_err());

    })).await;
}

#[tokio::test]
async fn candidate_reports_preserve_admissions_receipts_and_observation_failures() {
    let campaign = workspace_campaign_with(|authored| {
        crate::exomonad::edit_fixture_project_config(authored, |project| {
            project.haskell.modules = vec!["Project.Work".into()];
        });
    })
    .await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                committed(
                    root.as_ref(),
                    "import Project.WorkflowExamplesChecks\nworkflowContracts",
                )
                .await;
            })
        })
        .await;
}

#[tokio::test]
async fn project_review_retains_evidence_and_owns_direct_repair() {
    let campaign = workspace_campaign().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    campaign._repository.writer().stage(".exomonad").unwrap();
    let source = campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    displayed(
        campaign,
        root.as_ref(),
        &format!("let sourceHead = GitOid \"{}\"", source.as_str()),
    )
    .await;
    displayed(
        campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_delivery_setup.hs"),
    )
    .await;
    let (implementer, _implementer_binding) = next_project_worker(campaign).await;
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
        campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_review_start.hs"),
    )
    .await;
    let (reviewer, _reviewer_binding) = next_project_worker(campaign).await;
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
    let evidence = displayed(campaign, reviewer.policy.as_ref(), "_ <- display . show $ (reviewInput sessionInput)\nlet RetainedImplementer repairTarget = repairOwner sessionInput\n_ <- display . show $ (agentIdentity repairTarget)").await;
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
        campaign,
        reviewer.policy.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_review_repair.hs"),
    )
    .await;
    let pending = displayed(
        campaign,
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
    let repair_packet = displayed(campaign,
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
        campaign,
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
    let result = displayed(campaign, reviewer.policy.as_ref(), "state <- pollWatch repaired\n_ <- display . show $ fmap (either (const False) (const True)) state").await;
    assert_eq!(
        explicit_item_display_text(&result, 1),
        "WatchReady True",
        "{result}"
    );
    displayed(
        campaign,
        reviewer.policy.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/project_design_question.hs"),
    )
    .await;
    let (expert, _expert_binding) = next_project_worker(campaign).await;
    assert_eq!(expert.model.as_deref(), Some("planner"));
    assert_eq!(expert.fork_effort, Some(exomonad_actor::ForkEffort::Medium));
    assert_eq!(expert.supervisor_parent, Some(reviewer.actor.identity()));
    assert_eq!(expert.context_parent, None);
    let question = displayed(
        campaign,
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
    let decision = displayed(campaign, reviewer.policy.as_ref(), "design <- pollWatch designReady\n_ <- display . show $ design\n_ <- pollReply sessionReply >>= display . show").await;
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
        campaign,
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
        campaign,
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
    let checked = displayed(campaign, reviewer.policy.as_ref(), "incorporation <- pollWatch planReady\n_ <- display . show $ incorporation\n_ <- pollReply sessionReply >>= display . show").await;
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
        campaign,
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
        campaign,
        root.as_ref(),
        &format!(
            "let incorporatedHead = GitOid \"{}\"",
            incorporated_head.trimmed()
        ),
    )
    .await;
    let pending = displayed(
        campaign,
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
        campaign,
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
        campaign,
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
    let (consumer, _consumer_binding, activation) = next_project_activation(campaign).await;
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
        campaign,
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
        campaign,
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
        campaign,
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
})).await;
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
    let campaign = workspace_campaign().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    campaign._repository.writer().stage(".exomonad").unwrap();
    let source = campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    displayed(
        campaign,
        root.as_ref(),
        "let routeCampaign = \"route-reply\" :: Text",
    )
    .await;
    displayed(
        campaign,
        root.as_ref(),
        &format!("let sourceHead = GitOid \"{}\"", source.as_str()),
    )
    .await;
    displayed(
        campaign,
        root.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/route-reply-setup.hs"),
    )
    .await;
    let (lead, _lead_binding) = next_project_worker(campaign).await;
    displayed(
        campaign,
        lead.policy.as_ref(),
        &tidepool_testing::fixture_source(".exomonad/workspace/checks/route-reply-worker.hs"),
    )
    .await;
    let (worker, _worker_binding) = next_project_worker(campaign).await;
    if cancel {
        let result = displayed(
            campaign,
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
                    campaign,
                    lead.policy.as_ref(),
                    "_ <- pollRoute forwarding >>= display . show",
                )
                .await
            } else {
                displayed(
                    campaign,
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
            campaign, lead.policy.as_ref(),
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
            campaign,
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
})).await;
}

#[tokio::test]
async fn frozen_prompt_bytes_round_trip_through_haskell() {
    let campaign = workspace_campaign_with(|authored| {
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
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let result = displayed(
                    campaign,
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
            })
        })
        .await;
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

/// `exomonad check --workspace` accepts a spec requiring an effect in the
/// default root capabilities.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_check_accepts_a_spec_requiring_journal() {
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
    crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .expect("Journal is available to the root installer");
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_check_accepts_sleep_required_by_the_root_spec_installer() {
    let repository = recipe_workspace(None);
    crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .expect("the original spec installs with root capabilities");
    std::fs::write(
        repository.path().join(".exomonad/AgentSpec.hs"),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/sleep_required_spec.hs",
        ),
    )
    .unwrap();
    super::test_campaign::commit_workspace(repository.path());
    crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .expect("Sleep is available to the root installer");
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
async fn workspace_profile_executes_scopes_and_sleep_with_its_installed_effects() {
    let campaign = workspace_campaign_with(|authored| {
        crate::exomonad::edit_fixture_project_config(authored, |project| {
            project.haskell.modules.clear();
        });
    })
    .await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        include_str!(
            "../../../../exomonad/examples/workspace/.exomonad/checks/workspace-quantities.hs"
        ),
    )
    .await;
    committed(
        root.as_ref(),
        r#"if not (null workspaceProfileKeys) && all (\keys -> "ResourceScopes" `elem` keys && "Sleep" `elem` keys) workspaceProfileKeys then pure () else error "the installed workspace notebooks lost scope or sleep""#,
    )
    .await;
    committed(
        root.as_ref(),
        r#"if scopeBody workspaceSleepOutcome == Right () && scopeCleanup workspaceSleepOutcome == Right () then pure () else error "WorkspaceEffects scoped sleep did not complete with confirmed cleanup""#,
    )
    .await;
})).await;
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
    let campaign = TestCampaign::start_with_config(
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
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    displayed(
        campaign,
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
    let wrong_owner = displayed(campaign, root.as_ref(),
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
    let replaced = displayed(campaign, root.as_ref(),
        "collector <- R.replace collector (workDefinition sources (notifyWork owner (workMessage id)))\n_ <- R.call (workNotification (R.client collector)) receipt >>= display . show"
    ).await;
    assert!(
        replaced.to_string().contains("NotificationUnauthorized"),
        "{replaced}"
    );
    let retained = displayed(
        campaign,
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
})).await;
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

    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
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
        request = backend_request(campaign) => request.supply(Ok(backend.clone())),
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

    campaign.observe_shutdown().await.unwrap();
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

    })).await;
}

#[tokio::test]
async fn explicit_display_rejects_callback_effect_before_authority_input() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
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
            })
        })
        .await;
}

#[tokio::test]
async fn explicit_display_respects_shared_budget_and_handles_survive_cell_failure() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
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
})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hosted_parser_rejection_preserves_authored_diagnostics_without_publication() {
    use super::hosted_test_context::HostedTestRuntime;
    use super::test_campaign::{
        hosted_script_provider, hosted_test_settings, next_hosted_script_round,
    };
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start(&settings, &provider)
        .await
        .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Exercise authored diagnostics through the installed notebook.")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = std::collections::VecDeque::new();
            let mut round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            for (call, source, line) in [
                ("raw-parse", ":}", 1),
                (
                    "respond-parse",
                    "parserMustNotPublish <- pure (7 :: Int)\nrespond (",
                    2,
                ),
                ("missing-name", "parserMustNotPublish", 1),
            ] {
                round.call(call, source);
                round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
                let output = round.settled_output(call);
                assert_eq!(output["status"], "rejected", "{output}");
                let items = output["items"]
                    .as_array()
                    .expect("source rejection has receipts");
                assert!(!items.is_empty(), "{output}");
                let diagnostics = items
                    .iter()
                    .flat_map(|item| item["diagnostics"].as_array().into_iter().flatten())
                    .collect::<Vec<_>>();
                assert!(
                    diagnostics.iter().any(|diagnostic| {
                        diagnostic["message"].as_str().is_some_and(|text| {
                            !text.trim().is_empty()
                                && (call != "missing-name" || text.contains("parserMustNotPublish"))
                        }) && diagnostic["location"]["kind"] == "authored"
                            && diagnostic["location"]["label"] == "<cell>"
                            && diagnostic["location"]["startLine"] == line
                    }),
                    "{output}"
                );
                assert!(
                    items.iter().all(|item| item["installedBindings"]
                        .as_array()
                        .is_none_or(Vec::is_empty)
                        && item["operations"].as_array().is_none_or(Vec::is_empty)),
                    "{output}"
                );
            }
            round.call("after-rejection", "display ((1 :: Int) + 1)");
            let done = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            done.assert_value("after-rejection", "2");
            done.finish();
        })
    })
    .await;
}
