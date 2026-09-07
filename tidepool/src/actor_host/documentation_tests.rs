//! Focused execution of the published examples through the real resident tool.

use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;
use tidepool_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

fn example(document: &str) -> &str {
    examples(document).next().unwrap()
}

fn examples(document: &str) -> impl Iterator<Item = &str> {
    document
        .split("```haskell\n")
        .skip(1)
        .map(|block| block.split_once("```").unwrap().0)
}

/// Check the reference signatures from the published guide itself, without
/// maintaining a second list of API types in a fixture.
fn guide_signature_query(document: &str) -> String {
    let mut signatures: Vec<(String, String)> = Vec::new();
    for block in document.split("```text\n").skip(1) {
        for line in block.split_once("```").unwrap().0.lines() {
            if let Some((name, signature)) = line.split_once("::") {
                signatures.push((name.trim().into(), signature.trim().into()));
            } else if line.trim_start().starts_with("=>") || line.trim_start().starts_with("->") {
                let (_, signature) = signatures.last_mut().unwrap();
                signature.push(' ');
                signature.push_str(line.trim());
            }
        }
    }
    assert!(!signatures.is_empty(), "guide has no signature references");
    format!(
        ":type ({})",
        signatures
            .iter()
            .map(|(name, signature)| format!("({name} :: {signature})"))
            .collect::<Vec<_>>()
            .join(", ")
    )
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
) -> Arc<dyn tidepool_actor::ForkWorkspaceCustody> {
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
    let mut snippets = examples(include_str!("../../../prompts/shoal/base.md"));
    committed(root.as_ref(), snippets.next().unwrap()).await;
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
    let original = committed(
        root.as_ref(),
        "original <- pollResponse (forkedResponse worker)\ninspectFull original",
    )
    .await;
    assert!(original["items"][1]["output"]
        .as_str()
        .unwrap()
        .starts_with("ResponseReady"));
    let projected = committed(root.as_ref(), snippets.next().unwrap()).await;
    assert!(projected["items"][4]["output"]
        .as_str()
        .unwrap()
        .contains("ResponseReady"));
    campaign.await_watch_ready().await;
    let counted = committed(root.as_ref(), snippets.next().unwrap()).await;
    assert_eq!(counted["items"][1]["output"], "WatchReady 1");
    let stopped = committed(root.as_ref(), snippets.next().unwrap()).await;
    assert_eq!(stopped["items"][1]["output"], "[StoppedNow]");
    assert!(snippets.next().is_none(), "untested base-prompt example");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn shared_api_guide_example_handles_success_and_unavailable() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let guide = include_str!("../../../prompts/shoal/api-guide.md");
    let mut guide_examples = examples(guide);
    let signatures = committed(root.as_ref(), &guide_signature_query(guide)).await;
    // Diagnostic errors do not reject a whole tool block; inspect the unit too.
    assert_eq!(
        signatures["items"][0]["status"], "committed",
        "{signatures}"
    );
    committed(root.as_ref(), guide_examples.next().unwrap()).await;
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
    let success = committed(root.as_ref(), guide_examples.next().unwrap()).await;
    assert!(guide_examples.next().is_none(), "untested guide example");
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
        "state <- pollWatch retainedFailureReady\ninspectFull (fmap (either (const True) (const False) . reportOnly) state)",
    )
    .await;
    assert_eq!(unavailable["items"][1]["output"], "WatchReady True");
    let outer_unavailable = committed(
        root.as_ref(),
        "state <- pollWatch outerFailureReady\ninspectFull (guideIsUnavailable state)",
    )
    .await;
    assert_eq!(outer_unavailable["items"][1]["output"], "True");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn watch_documentation_request_options_reports_progress_then_settles() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        "lead <- startAgent (readonlyAgent \"documented-progress-lead\")",
    )
    .await;
    let snippets: Vec<_> = examples(include_str!("../../../prompts/shoal/docs/watch.md"))
        .filter(|snippet| snippet.contains("let progressOptions = requestOptions"))
        .collect();
    assert_eq!(snippets.len(), 1, "one complete documented progress setup");
    committed(root.as_ref(), snippets[0]).await;
    let child = tokio::time::timeout(Duration::from_secs(120), async {
        let mut child = None;
        loop {
            match campaign.deployments.recv().await {
                Some(LocalResidentDeployment::PolicyInstalled(installation)) => {
                    child = Some(installation)
                }
                Some(LocalResidentDeployment::SessionReady { activation }) => {
                    assert!(activation
                        .message
                        .contains("Publish cumulative findings; then return your final report."));
                    return child.expect("policy installed before request activation");
                }
                Some(_) => {}
                None => panic!("deployment channel closed"),
            }
        }
    })
    .await
    .expect("documented progress request activation");
    // Exercise the real resident actor path, without a model/provider execution claim.
    committed(child.policy.as_ref(), "reportProgress [\"finding\"]").await;
    campaign.await_watch_ready().await;
    let progress = committed(
        root.as_ref(),
        include_str!("watch_documentation_progress.hs"),
    )
    .await;
    assert_eq!(progress["items"][2]["output"], "True", "{progress}");
    assert_eq!(progress["items"][3]["output"], "True", "{progress}");
    let reply =
        dispatch_haskell_script(child.policy.as_ref(), "respond (\"final report\" :: Text)").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    let settled = committed(
        root.as_ref(),
        include_str!("watch_documentation_settled.hs"),
    )
    .await;
    assert_eq!(settled["items"][2]["output"], "True", "{settled}");
    assert_eq!(settled["items"][3]["output"], "True", "{settled}");
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
                    "the consumer explicitly requests Low; the domain leaves selection to the host Low default"
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

#[tokio::test]
async fn floating_point_resident_display_matches_prelude() {
    let campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let setup =
        dispatch_haskell_script(root.as_ref(), include_str!("numeric_display_fixture.hs")).await;
    let mut mismatches = Vec::new();
    let setup_committed = setup["status"] == "committed";
    if !setup_committed {
        mismatches.push(format!("fixture setup rejected: {setup}"));
    }
    // Keep observations separate so failures name the actual public display path.
    let probes = [
        ("d", "1.0"),
        ("f", "1.0"),
        ("inspectFull d", "1.0"),
        ("inspectFull f", "1.0"),
        ("(d, f)", "(1.0,1.0)"),
        ("[d, d]", "[1.0,1.0]"),
        ("[f, f]", "[1.0,1.0]"),
        ("NumericRecord d f", "NumericRecord 1.0 1.0"),
        ("inspectFull (NumericRecord d f)", "NumericRecord 1.0 1.0"),
        ("P.show (d, f)", "\"(1.0,1.0)\""),
        ("P.show [d, d]", "\"[1.0,1.0]\""),
        ("P.show [f, f]", "\"[1.0,1.0]\""),
        ("P.show (NumericRecord d f)", "\"NumericRecord 1.0 1.0\""),
        // The default Render path is separate and cannot stand in for Show.
        ("show d", "\"1.0\""),
        ("show f", "\"1.0\""),
        (
            "(P.isNaN d, P.isInfinite d, P.isNegativeZero d)",
            "(False,False,False)",
        ),
        (
            "(P.isNaN f, P.isInfinite f, P.isNegativeZero f)",
            "(False,False,False)",
        ),
    ];
    for (source, expected) in probes.into_iter().filter(|_| setup_committed) {
        let result = dispatch_haskell_script(root.as_ref(), source).await;
        if result["status"] != "committed" {
            mismatches.push(format!("{source}: rejected observation: {result}"));
            continue;
        }
        let Some(actual) = result["items"][0]["output"].as_str() else {
            mismatches.push(format!(
                "{source}: malformed committed observation: {result}"
            ));
            continue;
        };
        if actual != expected {
            mismatches.push(format!("{source}: expected {expected:?}, got {actual:?}"));
        }
    }
    campaign.forest.shutdown().await;
    if let Err(error) = campaign.hosted.await {
        mismatches.push(format!("hosted campaign join failed: {error}"));
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[tokio::test]
async fn model_selection_is_independent_of_inherited_and_selected_context() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(root.as_ref(), include_str!("fixtures/model_context.hs")).await;
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    tokio::time::timeout(Duration::from_secs(120), async {
        let mut ready = 0;
        while ready != 2 {
            match campaign.deployments.recv().await.unwrap() {
                LocalResidentDeployment::PolicyInstalled(child) => {
                    bindings.push(open_test_fork(&campaign, &child));
                    children.push(child);
                }
                LocalResidentDeployment::SessionReady { .. } => ready += 1,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    for child in &children {
        assert_eq!(child.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(child.supervisor_parent, Some(campaign.actor.identity()));
        if child.label.ends_with("/exact") {
            assert_eq!(child.context_parent, Some(campaign.actor.identity()));
            let inherited = committed(child.policy.as_ref(), "inspectFull parentOnly").await;
            assert_eq!(inherited["items"][0]["output"], "41");
        } else {
            assert_eq!(child.context_parent, None);
            assert_eq!(child.fork_effort, Some(tidepool_actor::ForkEffort::Medium));
            let missing = dispatch_haskell_script(child.policy.as_ref(), ":type parentOnly").await;
            assert!(missing.to_string().contains("not in scope"), "{missing}");
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
    committed(root.as_ref(), include_str!("fixtures/route.hs")).await;
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    tokio::time::timeout(Duration::from_secs(120), async {
        let mut ready = 0;
        while ready != 2 {
            match campaign.deployments.recv().await.unwrap() {
                LocalResidentDeployment::PolicyInstalled(child) => {
                    bindings.push(open_test_fork(&campaign, &child));
                    children.push(child);
                }
                LocalResidentDeployment::SessionReady { .. } => ready += 1,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
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
    let reviewer = tokio::time::timeout(Duration::from_secs(120), async {
        let mut reviewer = None;
        let mut forwarded = false;
        let mut review_ready = false;
        let mut failure_notified = false;
        loop {
            match campaign.deployments.recv().await.unwrap() {
                LocalResidentDeployment::PolicyInstalled(child) => {
                    assert_eq!(child.context_parent, None);
                    assert_eq!(child.model.as_deref(), Some("gpt-5.6-sol"));
                    bindings.push(open_test_fork(&campaign, &child));
                    reviewer = Some(child);
                }
                LocalResidentDeployment::SessionReady { activation } => {
                    if activation.message.contains("review-candidate") {
                        forwarded = true;
                    } else {
                        review_ready = true;
                    }
                }
                LocalResidentDeployment::WatchChanged { notification }
                    if notification.owner == campaign.actor.identity() =>
                {
                    let tidepool_actor::WatchTransition::RouteFailed { detail } =
                        notification.transition
                    else {
                        panic!("successful route woke a model: {notification:?}");
                    };
                    assert!(detail.contains("deliberate route failure"), "{detail}");
                    assert!(!failure_notified, "route failure notified twice");
                    failure_notified = true;
                }
                _ => {}
            }
            if forwarded && review_ready && failure_notified && reviewer.is_some() {
                break reviewer.unwrap();
            }
        }
    })
    .await
    .unwrap();
    let review = dispatch_haskell_script(reviewer.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(review["status"], "replied", "{review}");
    let state = committed(root.as_ref(), "pollRoute forwarding\npollRoute broken").await;
    assert_eq!(state["items"][0]["output"], "RouteCompleted");
    assert!(
        state["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains("deliberate route failure"),
        "{state}"
    );
    let reply = dispatch_haskell_script(consumer.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    committed(root.as_ref(), "stopAgent (forkedActor producer)").await;
    committed(root.as_ref(), "unavailable <- requestWith @Text (forkedActor producer) (requestOptions forwardedLabel (\"lost target\" :: Text))\nhandled <- route (awaitSettled unavailable) (\\settled -> case settled of { ReplyUnavailable _ -> pure (); ReplyAvailable _ -> error \"unexpected success\" })").await;
    let handled = committed(
        root.as_ref(),
        "pollRoute handled\nforgetRoute forwarding\nforgetRoute broken",
    )
    .await;
    assert_eq!(handled["items"][0]["output"], "RouteCompleted", "{handled}");
    assert_eq!(handled["items"][1]["output"], "WatchForgotten", "{handled}");
    assert_eq!(handled["items"][2]["output"], "WatchForgotten", "{handled}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn configured_modules_are_available_to_resident_declarations_from_frozen_sources() {
    let campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(), |admission| admission, |config| {
            let authored = config.workspace.join(".shoal");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            std::fs::write(authored.join("config.toml"), "[defaults]\nmodel = 'gpt-5.6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Types', 'Project.Work']\n").unwrap();
            std::fs::write(authored.join("Project/Types.hs"), include_str!("fixtures/project/Types.hs")).unwrap();
            std::fs::write(authored.join("Project/Work.hs"), include_str!("fixtures/project/Work.hs")).unwrap();
            config.workspace_inputs = Some(crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root).unwrap());
            std::fs::write(authored.join("Project/Work.hs"), "invalid edited source").unwrap();
        },
    ).await;
    let policy = campaign.root_installation.policy.as_ref();
    let result = committed(policy, "saved <- pure candidate\ninspectFull saved").await;
    assert_eq!(result["items"][1]["output"], "Preparation 7");
    let declaration = committed(policy, ":{\nreadDelivery :: Delivery -> Int\nreadDelivery (Preparation n) = n\nreadDelivery (Complete n) = n\n:}\ninspectFull (readDelivery candidate)").await;
    assert_eq!(declaration["items"][1]["output"], "7");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

async fn workspace_campaign() -> TestCampaign {
    TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".shoal");
            for (path, content) in [
                (
                    "config.toml",
                    include_str!("../../../examples/shoal-workspace/.shoal/config.toml"),
                ),
                (
                    "Project/Types.hs",
                    include_str!("../../../examples/shoal-workspace/.shoal/Project/Types.hs"),
                ),
                (
                    "Project/Work.hs",
                    include_str!("../../../examples/shoal-workspace/.shoal/Project/Work.hs"),
                ),
                (
                    "prompts/task.md",
                    include_str!("../../../examples/shoal-workspace/.shoal/prompts/task.md"),
                ),
                (
                    "prompts/lead.md",
                    include_str!("../../../examples/shoal-workspace/.shoal/prompts/lead.md"),
                ),
                (
                    "prompts/review.md",
                    include_str!("../../../examples/shoal-workspace/.shoal/prompts/review.md"),
                ),
                (
                    "prompts/repair.md",
                    include_str!("../../../examples/shoal-workspace/.shoal/prompts/repair.md"),
                ),
                (
                    "prompts/integrate.md",
                    include_str!("../../../examples/shoal-workspace/.shoal/prompts/integrate.md"),
                ),
                (
                    "prompts/core.md",
                    include_str!("../../../examples/shoal-workspace/.shoal/prompts/core.md"),
                ),
            ] {
                let target = authored.join(path);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, content).unwrap();
            }
            config.workspace_inputs = Some(
                crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                    .unwrap(),
            );
        },
    )
    .await
}

#[tokio::test]
async fn workspace_recipe_modules_and_snapshot_helpers_compile() {
    let campaign = workspace_campaign().await;
    let policy = campaign.root_installation.policy.as_ref();
    let result = committed(policy, "observed <- snapshot\ninspectFull (swarmUsage observed)\n:type (implement, reviewCandidate, integrateReviewed)").await;
    assert!(
        result["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains("unknownActors = [("),
        "{result}"
    );
    assert_eq!(result["items"][2]["status"], "committed", "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

async fn next_project_worker(
    campaign: &mut TestCampaign,
) -> (
    tidepool_actor::LocalResidentInstallation,
    Arc<dyn tidepool_actor::ForkWorkspaceCustody>,
) {
    tokio::time::timeout(Duration::from_secs(120), async {
        let mut worker = None;
        loop {
            match campaign.deployments.recv().await.unwrap() {
                LocalResidentDeployment::PolicyInstalled(child) => {
                    let binding = open_test_fork(campaign, &child);
                    worker = Some((child, binding));
                }
                LocalResidentDeployment::SessionReady { activation }
                    if worker.as_ref().is_some_and(|(child, _)| {
                        child.actor.identity() == activation.id.actor()
                    }) =>
                {
                    return worker.unwrap();
                }
                LocalResidentDeployment::WatchChanged { notification } => {
                    if let tidepool_actor::WatchTransition::RouteFailed { detail } =
                        notification.transition
                    {
                        panic!("route failed while awaiting worker admission: {detail}");
                    }
                }
                LocalResidentDeployment::Retired { actor, terminal } => {
                    panic!("actor {actor:?} retired while awaiting worker admission: {terminal:?}");
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn project_review_retains_evidence_and_owns_direct_repair() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".shoal").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        include_str!("fixtures/project_delivery_setup.hs"),
    )
    .await;
    let (implementer, _implementer_binding) = next_project_worker(&mut campaign).await;
    assert_eq!(
        implementer.instructions.as_deref(),
        Some(include_str!(
            "../../../examples/shoal-workspace/.shoal/prompts/task.md"
        ))
    );
    let tree = campaign
        .worktrees
        .lookup(&tidepool_worktree::WorktreeId::from_raw(
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
            "respond (Candidate \"{}\" [\"focused candidate check\"] [\"open product gate\"])",
            candidate.as_str()
        ),
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    let (reviewer, _reviewer_binding) = next_project_worker(&mut campaign).await;
    let review_instructions =
        include_str!("../../../examples/shoal-workspace/.shoal/prompts/review.md");
    assert_eq!(reviewer.instructions.as_deref(), Some(review_instructions));
    let launched = super::developer_instructions_selected(
        &reviewer.effective_role,
        &tidepool_agent::InteractiveLaunchMode::Fresh,
        None,
        reviewer.instructions.as_deref(),
    );
    assert!(launched.starts_with(review_instructions));
    assert!(launched.contains("Runtime policy ("));
    let evidence = committed(reviewer.policy.as_ref(), "inspectFull (reviewInput sessionInput)\ninspectFull (agentIdentity (reviewImplementer sessionInput))").await;
    assert!(
        evidence["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("focused candidate check"),
        "{evidence}"
    );
    assert!(
        evidence["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("open product gate"),
        "{evidence}"
    );
    assert_eq!(
        evidence["items"][1]["output"],
        format!(
            "({},{})",
            implementer.actor.identity().id.0,
            implementer.actor.identity().incarnation.0
        )
    );
    committed(
        reviewer.policy.as_ref(),
        include_str!("fixtures/project_review_repair.hs"),
    )
    .await;
    let pending = committed(reviewer.policy.as_ref(), "pollReply sessionReply").await;
    assert_eq!(pending["items"][0]["output"], "ReplyOpen", "{pending}");
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Some(LocalResidentDeployment::SessionReady { activation }) =
                campaign.deployments.recv().await
            {
                if activation.id.actor() == implementer.actor.identity() {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    let repair = committed(implementer.policy.as_ref(), "inspectFull sessionInput").await;
    assert!(
        repair["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("preserve the product gate"),
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
            "respond (Candidate \"{}\" [\"focused repair check\"] [\"open product gate\"])",
            revised.as_str()
        ),
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    let result = committed(reviewer.policy.as_ref(), "state <- pollWatch repaired\ninspectFull (fmap (either (const False) (const True) . repairValue) state)").await;
    assert_eq!(result["items"][1]["output"], "WatchReady True", "{result}");
    let original = committed(
        root.as_ref(),
        "original <- pollResponse (forkedResponse worker)\ninspectFull original",
    )
    .await;
    assert!(
        original["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains(candidate.as_str()),
        "{original}"
    );
    assert!(
        !original["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains(revised.as_str()),
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
    campaign._repository.writer().stage(".shoal").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(root.as_ref(), include_str!("fixtures/route_reply_setup.hs")).await;
    let (lead, _lead_binding) = next_project_worker(&mut campaign).await;
    committed(
        lead.policy.as_ref(),
        include_str!("fixtures/route_reply_worker.hs"),
    )
    .await;
    let (worker, _worker_binding) = next_project_worker(&mut campaign).await;
    if cancel {
        let result = committed(root.as_ref(), "cancelRequest (forkedResponse lead)").await;
        assert!(
            result.to_string().contains("CancellationRequested"),
            "{result}"
        );
    }
    let replied = dispatch_haskell_script(
        worker.policy.as_ref(),
        "respond (Candidate \"exact-candidate\" [\"checked\"] [\"open gate\"])",
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    // Observe the requester on success: the lead never needs a relay turn.
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = if cancel {
                committed(lead.policy.as_ref(), "pollRoute forwarding").await
            } else {
                committed(
                    root.as_ref(),
                    "answer <- pollResponse (forkedResponse lead)\ninspectFull answer",
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
        let pending = committed(lead.policy.as_ref(), "pollReply destination").await;
        assert!(
            pending.to_string().contains("ReplyCancellationRequested"),
            "{pending}"
        );
    } else {
        let response = outcome;
        let route = committed(lead.policy.as_ref(), "pollRoute forwarding").await;
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
async fn workspace_delivery_lane_integrates_partial_work_without_lead_relay() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".shoal").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        include_str!("fixtures/delivery_lane_setup.hs"),
    )
    .await;
    let (lead, _lead_binding) = next_project_worker(&mut campaign).await;
    committed(
        lead.policy.as_ref(),
        include_str!("fixtures/delivery_lane_worker.hs"),
    )
    .await;
    let (implementer, _implementation_binding) = next_project_worker(&mut campaign).await;
    let implementation_tree = campaign
        .worktrees
        .lookup(&tidepool_worktree::WorktreeId::from_raw(
            &implementer.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    let candidate = campaign
        ._repository
        .writer_at(implementation_tree.cwd())
        .commit_file("feature.txt", "prepared feature\n", "prepare feature")
        .unwrap();
    let result = dispatch_haskell_script(
        implementer.policy.as_ref(),
        &format!(
            "respond (Candidate \"{}\" [\"implementation check\"] [\"open product gate\"])",
            candidate.as_str(),
        ),
    )
    .await;
    assert_eq!(result["status"], "replied", "{result}");
    let (reviewer, _review_binding) = next_project_worker(&mut campaign).await;
    let review_tree = campaign
        .worktrees
        .lookup(&tidepool_worktree::WorktreeId::from_raw(
            &reviewer.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    let reviewed_head = campaign
        ._repository
        .writer_at(review_tree.cwd())
        .head()
        .unwrap();
    assert_eq!(reviewed_head, candidate);
    assert_eq!(
        std::fs::read_to_string(review_tree.cwd().join("feature.txt")).unwrap(),
        "prepared feature\n"
    );
    let result = dispatch_haskell_script(reviewer.policy.as_ref(), &format!(
        "respond (Accepted (ReviewedCandidate (reviewInput sessionInput) \"{}\" [\"review check\"] \"coherent preparation\"))", reviewed_head.as_str(),
    )).await;
    assert_eq!(result["status"], "replied", "{result}");
    let (integrator, _integration_binding) = next_project_worker(&mut campaign).await;
    let evidence = committed(integrator.policy.as_ref(), "inspectFull sessionInput").await;
    for expected in [
        candidate.as_str(),
        "implementation check",
        "review check",
        "coherent preparation",
        "open product gate",
    ] {
        assert!(
            evidence.to_string().contains(expected),
            "missing {expected}: {evidence}"
        );
    }
    let integration_tree = campaign
        .worktrees
        .lookup(&tidepool_worktree::WorktreeId::from_raw(
            &integrator.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    tidepool_worktree::GitCli::new()
        .try_run(
            integration_tree.cwd(),
            &["merge", "--ff-only", candidate.as_str()],
        )
        .unwrap();
    let integrated_head = campaign
        ._repository
        .writer_at(integration_tree.cwd())
        .head()
        .unwrap();
    assert_eq!(integrated_head, candidate);
    assert_eq!(
        std::fs::read_to_string(integration_tree.cwd().join("feature.txt")).unwrap(),
        "prepared feature\n"
    );
    let result = dispatch_haskell_script(integrator.policy.as_ref(), &format!(
        "respond (Preparation (Candidate \"{}\" [\"integration content check\"] (remainingGates (reviewedCandidate sessionInput))))", integrated_head.as_str(),
    )).await;
    assert_eq!(result["status"], "replied", "{result}");
    let delivery = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = committed(
                root.as_ref(),
                "delivered <- pollResponse (forkedResponse lead)\ninspectFull delivered",
            )
            .await;
            if result.to_string().contains("ResponseReady") {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    for expected in [
        "Preparation",
        integrated_head.as_str(),
        "integration content check",
        "open product gate",
    ] {
        assert!(
            delivery.to_string().contains(expected),
            "missing {expected}: {delivery}"
        );
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
