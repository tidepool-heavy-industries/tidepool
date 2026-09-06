//! Focused execution of the published examples through the real resident tool.

use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;
use tidepool_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

fn example(document: &str) -> &str {
    document
        .split_once("```haskell\n")
        .unwrap()
        .1
        .split_once("```")
        .unwrap()
        .0
}

async fn committed(
    policy: &dyn tidepool_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    let result = dispatch_haskell_script(policy, source).await;
    assert_eq!(result["status"], "committed", "{result:?}");
    result
}

fn open_test_fork(
    campaign: &TestCampaign,
    child: &tidepool_actor::LocalResidentInstallation,
) -> tidepool_worktree::ActiveBinding {
    campaign.authority.install_grant(
        child.actor.identity().into(),
        worktree_grant(child.effective_role.role()),
    );
    let [worktree_id] = child.launch_worktrees.as_slice() else {
        panic!("child must have one worktree")
    };
    let worktree = campaign
        .worktrees
        .lookup(&tidepool_worktree::WorktreeId::from_raw(worktree_id))
        .unwrap()
        .unwrap();
    let principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(campaign.session_root.path()),
        child.actor.identity().id.0,
        child.actor.identity().incarnation.0,
    );
    let binding = campaign
        .bindings
        .lock()
        .bind(worktree.id(), &principal, current_time_ms())
        .unwrap();
    child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    binding
}

#[tokio::test]
async fn published_unfold_watch_and_request_examples_execute() {
    execute_examples(false, None, CompletionAction::Acknowledge).await;
}

#[tokio::test]
async fn base_prompt_coordination_example_executes() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        "let seed = projectHead\nlet task = \"Check the hit targets.\" :: Text",
    )
    .await;
    committed(
        root.as_ref(),
        example(include_str!("../../../prompts/shoal/base.md")),
    )
    .await;
    let mut binding = None;
    let child = tokio::time::timeout(Duration::from_secs(120), async {
        let mut child = None;
        loop {
            match campaign.deployments.recv().await {
                Some(LocalResidentDeployment::PolicyInstalled(installation)) => {
                    binding = Some(open_test_fork(&campaign, &installation));
                    child = Some(installation)
                }
                Some(LocalResidentDeployment::SessionReady { activation }) => {
                    assert!(activation.message.contains("Check the hit targets."));
                    return child.unwrap();
                }
                Some(_) => {}
                None => panic!("deployment stream closed"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(child.fork_effort, Some(tidepool_actor::ForkEffort::Low));
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    committed(root.as_ref(), "result <- pollWatch ready").await;
    let result = committed(root.as_ref(), "inspectFull result").await;
    assert!(result["items"][0]["output"]
        .as_str()
        .unwrap()
        .contains("Check the hit targets."));
    let original = committed(root.as_ref(), "pollResponse (forkedResponse worker)").await;
    assert!(original["items"][0]["output"]
        .as_str()
        .unwrap()
        .starts_with("ResponseReady"));
    committed(root.as_ref(), "stopAgent (forkedActor worker)").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn shared_api_guide_example_handles_success_and_unavailable() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        example(include_str!("../../../prompts/shoal/api-guide.md")),
    )
    .await;
    let mut binding = None;
    let child = tokio::time::timeout(Duration::from_secs(120), async {
        let mut child = None;
        loop {
            match campaign.deployments.recv().await {
                Some(LocalResidentDeployment::PolicyInstalled(installation)) => {
                    binding = Some(open_test_fork(&campaign, &installation));
                    child = Some(installation);
                }
                Some(LocalResidentDeployment::SessionReady { activation }) => {
                    assert!(activation.message.contains("Check the hit targets."));
                    return child.unwrap();
                }
                Some(_) => {}
                None => panic!("deployment stream closed"),
            }
        }
    })
    .await
    .unwrap();
    let pending = committed(root.as_ref(), "state <- pollWatch ready\ninspectFull state").await;
    assert_eq!(pending["items"][1]["output"], "WatchPending");
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    let success = committed(
        root.as_ref(),
        "state <- pollWatch ready\ninspectFull (fmap reportOnly state)",
    )
    .await;
    assert_eq!(
        success["items"][1]["output"],
        "WatchReady (Right \"Check the hit targets.\")"
    );

    committed(
        root.as_ref(),
        include_str!("shared_api_guide_unavailable.hs"),
    )
    .await;
    campaign.await_watch_ready().await;
    let unavailable = committed(
        root.as_ref(),
        "state <- pollWatch failureReady\ninspectFull (fmap (either (const True) (const False) . reportOnly) state)",
    )
    .await;
    assert_eq!(unavailable["items"][1]["output"], "WatchReady True");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn activation_presents_prose_and_preserves_exact_inputs() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let mut child = None;
    committed(root.as_ref(), "data Report = Report Int deriving Show\nworker <- startAgent (readonlyAgent \"activation-preview-worker\")").await;
    committed(
        root.as_ref(),
        include_str!("../actor_host_fixtures/generic_actor/activation_preview_setup.hs"),
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
            "preview-oversized-text",
            "oversizedTextPreview",
            "expand with `inspectFull sessionInput`",
            "respond (Report 1)",
        ),
        (
            "preview-opaque",
            "opaquePreview",
            "<opaque value>",
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
        committed(root.as_ref(), &format!("let Right previewLabel = requestLabel \"{label}\"\npreviewResponse <- request @Report worker previewLabel {input}")).await;
        let activation = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                match campaign.deployments.recv().await {
                    Some(LocalResidentDeployment::PolicyInstalled(installation)) => {
                        child = Some(installation)
                    }
                    Some(LocalResidentDeployment::SessionReady { activation })
                        if activation.message.contains(label) =>
                    {
                        break activation
                    }
                    Some(_) => {}
                    None => panic!("deployment stream closed"),
                }
            }
        })
        .await
        .expect("preview activation");
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
        if label == "preview-long-text" {
            assert!(
                !activation.message.contains("omitted"),
                "{}",
                activation.message
            );
            let observation =
                committed(child.as_ref().unwrap().policy.as_ref(), "sessionInput").await;
            assert!(!observation["items"][0]["output"]
                .as_str()
                .unwrap()
                .contains("FINAL-ACCEPTANCE-CONDITION"));
        }
        if label == "preview-oversized-text" {
            assert!(activation.message.len() < 17 * 1024);
            assert!(!activation.message.contains("RETAINED-ASSIGNMENT-TAIL"));
            let expanded = committed(
                child.as_ref().unwrap().policy.as_ref(),
                "inspectFull sessionInput",
            )
            .await;
            assert!(
                expanded["items"][0]["output"]
                    .as_str()
                    .unwrap()
                    .contains("RETAINED-ASSIGNMENT-TAIL"),
                "{expanded}"
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
    struct VerifyHeap;
    impl Drop for VerifyHeap {
        fn drop(&mut self) {
            tidepool_codegen::host_fns::clear_heap_verify_override();
        }
    }
    let before = tidepool_codegen::host_fns::heap_verify_run_count();
    tidepool_codegen::host_fns::set_heap_verify(true);
    let _verification = VerifyHeap;
    execute_examples(true, None, CompletionAction::Acknowledge).await;
    assert!(tidepool_codegen::host_fns::heap_verify_run_count() > before);
}

#[tokio::test]
async fn quiet_observation_retains_exact_results_without_repeating_effects() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(root.as_ref(), include_str!("quiet_observation_setup.hs")).await;
    let child = tokio::time::timeout(Duration::from_secs(60), async {
        let mut child = None;
        loop {
            match campaign.deployments.recv().await {
                Some(LocalResidentDeployment::PolicyInstalled(installation)) => {
                    child = Some(installation)
                }
                Some(LocalResidentDeployment::SessionReady { .. }) => return child.unwrap(),
                Some(_) => {}
                None => panic!("deployment stream closed"),
            }
        }
    })
    .await
    .unwrap();
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    let first = committed(root.as_ref(), "pollWatch ready").await;
    let saved = first["items"][0]["installedBindings"][0].as_str().unwrap();
    let output = first["items"][0]["output"].as_str().unwrap();
    assert!(output.starts_with("WatchReady"), "{first}");
    assert!(output.len() < 800, "{first}");
    assert!(
        output.contains(&format!("inspectFull ({saved} ())")),
        "{first}"
    );
    let full = committed(root.as_ref(), &format!("inspectFull ({saved} ())")).await;
    let full_text = full["items"][0]["output"].as_str().unwrap();
    assert!(full_text.contains("candidate-9828") && full_text.contains("tested-6c6c"));
    assert!(full_text.contains("LIMITATION-MUST-REMAIN-AVAILABLE"));
    assert!(full_text.len() > 50_000);
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
    let retained = committed(root.as_ref(), "inspectFull retained").await;
    assert_eq!(retained["items"][0]["output"], full["items"][0]["output"]);
    let declared = committed(root.as_ref(), "inspectFull declaredEvidence").await;
    assert_eq!(declared["items"][0]["output"], full["items"][0]["output"]);
    let generic = committed(root.as_ref(), "delivery").await;
    assert!(generic["items"][0]["output"].as_str().unwrap().len() < 900);
    let infinite = committed(root.as_ref(), "repeat 'x'").await;
    assert!(infinite["items"][0]["output"].as_str().unwrap().len() < 900);
    let lifecycle = committed(root.as_ref(), "WatchReady (Costly 8)").await;
    assert!(lifecycle["items"][0]["output"]
        .as_str()
        .unwrap()
        .starts_with("WatchReady"));
    let broken = committed(root.as_ref(), "Costly 7").await;
    assert!(
        broken["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("preview unavailable"),
        "{broken}"
    );
    let broken_name = broken["items"][0]["installedBindings"][0].as_str().unwrap();
    let recovered = committed(
        root.as_ref(),
        &format!("case {broken_name} () of Costly n -> n"),
    )
    .await;
    assert_eq!(recovered["items"][0]["output"], "7");
    let before = campaign
        .forest
        .inspect_graph(campaign.actor.identity())
        .unwrap()
        .len();
    let spawned = committed(root.as_ref(), "startAgent (readonlyAgent \"observe-once\")").await;
    let spawned_name = spawned["items"][0]["installedBindings"][0]
        .as_str()
        .unwrap();
    let inspect = format!("inspectFull (agentIdentity ({spawned_name} ()))");
    let one = committed(root.as_ref(), &inspect).await;
    let two = committed(root.as_ref(), &inspect).await;
    assert_eq!(one["items"][0]["output"], two["items"][0]["output"]);
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
    execute_examples(
        false,
        Some("missingBindingAfterSuccessfulUnfold"),
        CompletionAction::Acknowledge,
    )
    .await;
}

#[tokio::test]
async fn reattachment_cancels_unacknowledged_forks() {
    execute_examples(false, None, CompletionAction::Reattach { groups: 1 }).await;
}

#[tokio::test]
async fn multiple_unfolds_are_admitted_before_completion() {
    execute_examples(false, None, CompletionAction::Reattach { groups: 2 }).await;
}

enum CompletionAction {
    Acknowledge,
    Reattach { groups: usize },
}

async fn execute_examples(
    rich_response: bool,
    suffix: Option<&str>,
    completion_action: CompletionAction,
) {
    let mut campaign = TestCampaign::start().await;
    let root = Arc::clone(&campaign.root_installation.policy);
    for block in include_str!("../../../prompts/shoal/docs/workbench.md")
        .split("```haskell\n")
        .skip(1)
    {
        committed(root.as_ref(), block.split_once("```").unwrap().0).await;
    }
    committed(
        root.as_ref(),
        if rich_response {
            include_str!("../actor_host_fixtures/generic_actor/rich_response_setup.hs")
        } else {
            include_str!("../actor_host_fixtures/generic_actor/documentation_setup.hs")
        },
    )
    .await;
    committed(root.as_ref(), ":type (undefined :: Review)").await;
    let extra_group = if matches!(completion_action, CompletionAction::Reattach { groups: 2 }) {
        include_str!("../actor_host_fixtures/generic_actor/second_queued_unfold.hs")
    } else {
        ""
    };
    let call_id = "documentation-unfold".to_owned();
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        root.dispatch_boxed(ToolInvocation {
            context: Some(ToolInvocationContext {
                context_call_id: Some(call_id.clone()),
                thread_id: "actor-host-vertical".into(),
                turn_id: call_id.clone(),
                call_id: call_id.clone(),
                namespace: Some("haskell".into()),
            }),
            name: tidepool_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(format!(
                "{}\n{}\n{}\n{}",
                example(include_str!("../../../prompts/shoal/docs/unfold.md")),
                example(include_str!("../../../prompts/shoal/docs/watch.md")),
                extra_group,
                suffix.unwrap_or("")
            )),
        }),
    )
    .await
    .expect("unfold must return without provider startup")
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
    while let Ok(event) = campaign.deployments.try_recv() {
        assert!(
            !matches!(event, LocalResidentDeployment::PolicyInstalled(_)),
            "child started before tool completion"
        );
    }
    let completion = tidepool_runtime::session::WorkbenchForkBoundary {
        thread_id: "actor-host-vertical".into(),
        call_id,
    };
    if let CompletionAction::Reattach { groups } = completion_action {
        root.reattach_boxed().await.unwrap();
        root.complete_boxed(completion).await.unwrap();
        let mut retired = 0;
        tokio::time::timeout(Duration::from_secs(10), async {
            while retired < groups * 2 {
                match campaign.deployments.recv().await.unwrap() {
                    LocalResidentDeployment::Retired { terminal, .. } => {
                        assert_eq!(terminal.kind, ActorExitKind::Cancelled);
                        assert!(terminal
                            .summary
                            .contains("without acknowledging tool completion"));
                        retired += 1;
                    }
                    LocalResidentDeployment::PolicyInstalled(_) => {
                        panic!("cancelled child started")
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("queued children must settle on reattachment");
        campaign
            .actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "reattachment test complete".into(),
            })
            .await
            .unwrap();
        campaign.hosted.await.unwrap();
        return;
    }
    root.complete_boxed(completion.clone()).await.unwrap();
    root.complete_boxed(completion).await.unwrap();
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    let mut fork_boundary = None;
    let mut activation_messages = Vec::new();
    while children.len() < 2 {
        let event = tokio::time::timeout(Duration::from_secs(120), async {
            campaign
                .deployments
                .recv()
                .await
                .expect("deployment channel closed")
        })
        .await
        .expect("child deployment timed out");
        match event {
            LocalResidentDeployment::PolicyInstalled(child) => {
                let expected_effort = child
                    .label
                    .ends_with("/consumer-tests")
                    .then_some(tidepool_actor::ForkEffort::Low);
                assert_eq!(
                    child.fork_effort, expected_effort,
                    "the consumer example selects low effort; the domain inherits"
                );
                let boundary = child.fork_boundary.as_ref().expect("hosted fork boundary");
                assert_eq!(boundary.thread_id, "actor-host-vertical");
                assert!(!boundary.call_id.is_empty());
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
            LocalResidentDeployment::Retired { actor, terminal } => {
                panic!("{actor:?} retired: {terminal:?}")
            }
            LocalResidentDeployment::SessionReady { activation } => {
                activation_messages.push(activation)
            }
            _ => {}
        }
    }
    let mut presented = std::collections::HashSet::new();
    while presented.len() < children.len() {
        let event = if let Some(activation) = activation_messages.pop() {
            LocalResidentDeployment::SessionReady { activation }
        } else {
            tokio::time::timeout(Duration::from_secs(120), campaign.deployments.recv())
                .await
                .expect("request activation timed out")
                .expect("deployment channel closed")
        };
        if let LocalResidentDeployment::SessionReady { activation } = event {
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
    }
    committed(
        root.as_ref(),
        "let sharedAfterUnfold = (\"later parent value\" :: Text)",
    )
    .await;
    for child in &children {
        let inherited = committed(child.policy.as_ref(), "sharedAfterUnfold").await;
        assert_eq!(
            inherited["items"][0]["output"].as_str().unwrap().trim(),
            "ready"
        );
    }
    committed(
        root.as_ref(),
        include_str!("../actor_host_fixtures/generic_actor/response_computation.hs"),
    )
    .await;
    for child in &children {
        let source = if rich_response {
            include_str!("../actor_host_fixtures/generic_actor/rich_response_reply.hs")
        } else if child.label.ends_with("/domain") {
            "respond (Report sessionInput)"
        } else {
            "respond sessionInput"
        };
        let result = dispatch_haskell_script(child.policy.as_ref(), source).await;
        assert_eq!(result["status"], "replied", "{result:?}");
    }
    campaign.await_watch_ready().await;
    let first = committed(root.as_ref(), "pollWatch joined").await;
    let second = committed(root.as_ref(), "pollWatch joined").await;
    for observation in [&first, &second] {
        assert!(observation["items"][0]["output"]
            .as_str()
            .unwrap()
            .starts_with("WatchReady"));
    }
    let saved = first["items"][0]["installedBindings"][0].as_str().unwrap();
    let full = committed(root.as_ref(), &format!("inspectFull ({saved} ())")).await;
    assert!(full["items"][0]["output"]
        .as_str()
        .unwrap()
        .contains("ReplyAvailable"));
    committed(
        root.as_ref(),
        "context <- actorContext\ncontextFirstUsage context\ncontextLatestUsage context",
    )
    .await;
    committed(
        root.as_ref(),
        "let worker = forkedActor (fst workers)\nlet task = 9 :: Int",
    )
    .await;
    let domain = children
        .iter()
        .find(|child| child.label.ends_with("/domain"))
        .unwrap();
    for document in [
        include_str!("../../../prompts/shoal/docs/request.md"),
        include_str!("../../../prompts/shoal/docs/deadline.md"),
    ] {
        if rich_response {
            break;
        }
        committed(root.as_ref(), example(document)).await;
        committed(
            root.as_ref(),
            "let Right readyLabel = watchLabel \"followup-result\"\nready <- watch readyLabel (awaitResponse response)",
        )
        .await;
        let reply =
            dispatch_haskell_script(domain.policy.as_ref(), "respond (Report sessionInput)").await;
        assert_eq!(reply["status"], "replied", "{reply:?}");
        campaign.await_watch_ready().await;
        let result = committed(root.as_ref(), "pollResponse response").await;
        assert!(result["items"][0]["output"]
            .as_str()
            .unwrap()
            .starts_with("ResponseReady"));
        let saved = result["items"][0]["installedBindings"][0].as_str().unwrap();
        let full = committed(root.as_ref(), &format!("inspectFull ({saved} ())")).await;
        assert!(
            full["items"][0]["output"]
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
            })
            .await
            .unwrap();
    }
    let status = committed(root.as_ref(), ":status").await;
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
        })
        .await
        .unwrap();
    campaign.hosted.await.unwrap();
}
