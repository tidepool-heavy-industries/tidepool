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

#[tokio::test]
async fn published_unfold_watch_and_request_examples_execute() {
    execute_examples(false, None, CompletionAction::Acknowledge).await;
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
                let expected = if child.label.ends_with("/domain") {
                    Some(tidepool_actor::ForkEffort::Low)
                } else {
                    None
                };
                assert_eq!(child.fork_effort, expected);
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
                bindings.push(
                    campaign
                        .bindings
                        .lock()
                        .bind(worktree.id(), &principal, current_time_ms())
                        .unwrap(),
                );
                child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
                children.push(child);
            }
            LocalResidentDeployment::Retired { actor, terminal } => {
                panic!("{actor:?} retired: {terminal:?}")
            }
            _ => {}
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
    assert_eq!(first["items"][0]["output"], second["items"][0]["output"]);
    assert!(first["items"][0]["output"]
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
            .contains("Report 9"));
    }
    for child in children {
        child
            .actor
            .shutdown(ActorTerminal {
                kind: if child.label.ends_with("/review") {
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
