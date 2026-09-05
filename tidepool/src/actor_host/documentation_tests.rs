//! Focused execution of the published examples through the real resident tool.

use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;

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
    let mut campaign = TestCampaign::start().await;
    let root = Arc::clone(&campaign.root_installation.policy);
    committed(
        root.as_ref(),
        include_str!("../actor_host_fixtures/generic_actor/documentation_setup.hs"),
    )
    .await;
    committed(root.as_ref(), ":type (undefined :: Review)").await;
    let submitting = Arc::clone(&root);
    let mut unfold = tokio::spawn(async move {
        committed(
            submitting.as_ref(),
            example(include_str!("../../../prompts/shoal/docs/unfold.md")),
        )
        .await
    });
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    let mut fork_boundary = None;
    while children.len() < 2 {
        let event = tokio::time::timeout(Duration::from_secs(120), async {
            tokio::select! {
                event = campaign.deployments.recv() => event.expect("deployment channel closed"),
                result = &mut unfold => panic!("unfold ended before child readiness: {result:?}"),
            }
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
    tokio::time::timeout(Duration::from_secs(120), unfold)
        .await
        .unwrap()
        .unwrap();
    committed(
        root.as_ref(),
        example(include_str!("../../../prompts/shoal/docs/watch.md")),
    )
    .await;
    for child in &children {
        let source = if child.label.ends_with("/domain") {
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
