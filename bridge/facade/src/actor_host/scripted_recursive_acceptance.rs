//! Real recursive model actors and native captures; only provider replies are scripted.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    commit_workspace, explicit_display_text, hosted_script_provider, hosted_test_settings,
    next_hosted_script_round, require_ghc_compile_rejection, HostedScriptRound,
    COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
};
use super::*;
use harness::embedding::HostIdentity;
use harness::model::{AgentPath, CallId, OperationId, RequestId};
use serde_json::json;
use std::collections::VecDeque;

const SETUP: &str = include_str!("fixtures/m3_recursive_setup.hs");
const ROOT: &str = include_str!("fixtures/m3_recursive_root.hs");
const CHILD: &str = include_str!("fixtures/m3_recursive_child.hs");
const REFUSAL: &str = include_str!("fixtures/m3_recursive_grandchild_refusal.hs");
const GRANDCHILD_REPLY: &str = include_str!("fixtures/m3_recursive_grandchild_reply.hs");
const CHILD_REPLY: &str = include_str!("fixtures/m3_recursive_child_reply.hs");
const CLEANUP: &str = include_str!("fixtures/m3_recursive_cleanup.hs");
const ROOT_CALL: &str = "recursive-root-await";
const CHILD_CALL: &str = "recursive-child-await";

fn identity(round: &HostedScriptRound) -> HostIdentity {
    let harness::model::ConversationIdentity::Embedded {
        run,
        actor,
        incarnation,
    } = round.origin()
    else {
        panic!("hosted provider request must retain its embedded origin");
    };
    HostIdentity {
        run,
        actor,
        incarnation,
    }
}

async fn admit_http_input(host: &HostedTestRuntime) {
    let origin = format!("https://{}", host.address);
    let api = format!("http://{}/api", host.address);
    let client = reqwest::Client::builder()
        .timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET)
        .build()
        .unwrap();
    let login = client
        .post(format!("{api}/session"))
        .header("Origin", &origin)
        .json(&json!({"secret": "hosted-script-test-secret-32-bytes"}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = login.headers()[reqwest::header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let receipt = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, cookie)
        .json(&harness::server::ClientCommand::Host {
            operation_id: harness::embedding::ClientOperationId(uuid::Uuid::new_v4()),
            command: harness::server::HostCommand::Input {
                target: HostIdentity {
                    run: runtime_namespace(&host.context.config.run_directory.path()),
                    actor: AgentPath("/root".into()),
                    incarnation: host.context.actor.identity().incarnation.0.to_string(),
                },
                text: "Run the recursive captured-helper scenario.".into(),
            },
        })
        .send()
        .await
        .unwrap();
    assert_eq!(receipt.status(), reqwest::StatusCode::ACCEPTED);
}

fn operation(host: &HostedTestRuntime, round: &HostedScriptRound, call: &str) -> OperationId {
    OperationId {
        origin: round.origin(),
        request: host
            .runtime
            .store()
            .embedded_round_frontier(&identity(round))
            .unwrap()
            .pending_head
            .expect("held provider request has an exact pending frontier"),
        call: CallId(call.into()),
    }
}

async fn next(
    host: &HostedTestRuntime,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
    pending: &mut VecDeque<HostedScriptRound>,
    actor: &AgentPath,
) -> HostedScriptRound {
    host.context
        .while_root_live(
            "recursive provider barrier",
            next_hosted_script_round(requests, pending, actor),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"))
}

async fn next_descendant(
    host: &HostedTestRuntime,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
    pending: &mut VecDeque<HostedScriptRound>,
    parent: ActorRef,
) -> (ActorRef, HostedScriptRound) {
    let round = match pending.pop_front() {
        Some(round) => round,
        None => host
            .context
            .while_root_live(
                "recursive descendant provider barrier",
                tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, requests.recv()),
            )
            .await
            .unwrap_or_else(|error| panic!("{error}"))
            .expect("admitted descendant requests the provider")
            .expect("provider observer is live"),
    };
    let origin = identity(&round);
    let graph = host.context.forest.inspect_host_graph();
    let node = graph
        .iter()
        .find(|node| {
            host.context
                .binding(node.actor)
                .and_then(|binding| binding.conversation())
                .is_some_and(|conversation| conversation.identity() == &origin)
        })
        .expect("provider origin belongs to one actual hosted actor");
    assert_eq!(node.creator, Some(parent));
    assert_eq!(node.supervisor_parent, Some(parent));
    assert_eq!(node.context_parent, Some(parent));
    assert!(node.model_actor);
    assert!(node.bound_worktree.is_some());
    assert_eq!(round.request.model, "gpt-6-luna");
    (node.actor, round)
}

fn transcript(round: &HostedScriptRound) -> Vec<harness::item::Item> {
    round
        .request
        .input
        .iter()
        .filter(|item| !item.is_configuration_update())
        .cloned()
        .collect()
}

fn captured(round: &HostedScriptRound, prefix: &[harness::item::Item], excluded_call: &str) {
    let retained = transcript(round);
    assert!(
        retained.starts_with(prefix),
        "the exact parent provider prefix survives capture"
    );
    assert!(
        !retained
            .iter()
            .any(|item| item.0["call_id"] == excluded_call),
        "BeforeCall capture excludes the unfinished parent invocation"
    );
    assert!(retained
        .iter()
        .any(|item| item.0["call_id"] == "recursive-setup"));
}

fn pending_claim(host: &HostedTestRuntime, operation: &OperationId) {
    let claims = host.runtime.store().claims(&operation.call).unwrap();
    let exact = claims
        .iter()
        .filter(|claim| claim.operation == *operation && claim.request == operation.request)
        .collect::<Vec<_>>();
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].state, harness::store::ClaimState::Pending);
}

fn unsettled(round: &HostedScriptRound, call: &str) {
    assert!(!round.request.input.iter().any(|item| {
        item.0["call_id"] == call
            && matches!(
                item.0["type"].as_str(),
                Some("custom_tool_call_output" | "function_call_output")
            )
    }));
}

async fn parked(
    host: &HostedTestRuntime,
    actor: &exomonad_actor::LocalActorRef,
    operation: &OperationId,
) {
    let harness::model::ConversationIdentity::Embedded {
        run,
        actor: path,
        incarnation,
    } = &operation.origin
    else {
        panic!("embedded pending operation required")
    };
    let context = exomonad_tool::ToolInvocationContext {
        origin: exomonad_tool::ToolInvocationOrigin::Model(
            embedded_harness::original_operation(
                &HostIdentity {
                    run: run.clone(),
                    actor: path.clone(),
                    incarnation: incarnation.clone(),
                },
                operation,
            )
            .unwrap(),
        ),
        call_id: operation.call.0.clone(),
        namespace: None,
    };
    host.context
        .while_root_live(
            "recursive typed settlement is parked",
            tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                loop {
                    pending_claim(host, operation);
                    if actor.hosted_workbench_waiting(&context).is_some() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"))
        .expect("held descendant response keeps its ancestor's native Watch parked");
}

async fn yield_pending(
    host: &HostedTestRuntime,
    round: HostedScriptRound,
    call: &str,
    operation: &OperationId,
) {
    let request = issuing_request(host, &round);
    round.function(call, "yield", json!({}));
    host.context
        .while_root_live(
            "recursive Engine yield is persisted",
            tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                loop {
                    pending_claim(host, operation);
                    if host
                        .runtime
                        .store()
                        .replay_turns(&request)
                        .unwrap()
                        .iter()
                        .any(|turn| {
                            turn.request == request
                                && turn.model_response.items.iter().any(|item| {
                                    item.0["type"] == "function_call"
                                        && item.0["name"] == "yield"
                                        && item.0["call_id"] == call
                                })
                        })
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"))
        .expect("yield is recorded before a scripted descendant response is released");
    assert!(host
        .runtime
        .store()
        .claims(&CallId(call.into()))
        .unwrap()
        .is_empty());
}

fn issuing_request(host: &HostedTestRuntime, round: &HostedScriptRound) -> RequestId {
    host.runtime
        .store()
        .embedded_round_frontier(&identity(round))
        .unwrap()
        .pending_head
        .expect("provider request has its issuing frontier")
}

fn replied(round: &HostedScriptRound, call: &str) {
    let receipt = round.settled_output(call);
    assert_eq!(receipt["status"], "replied", "{receipt}");
    assert_eq!(receipt["publication"]["status"], "published", "{receipt}");
    let items = receipt["items"].as_array().unwrap();
    assert!(
        items.iter().all(|item| item["status"] == "committed"),
        "{receipt}"
    );
    assert_eq!(
        items.last().unwrap()["terminalTransfer"],
        "replyAccepted",
        "{receipt}"
    );
}

async fn stopped(host: &HostedTestRuntime, actor: ActorRef, parent: ActorRef) {
    let installation = host.context.observer.installation(actor).await;
    assert!(installation.checkpoint);
    assert_eq!(installation.context_parent, Some(parent));
    let terminal = installation.actor.terminal();
    tokio::time::timeout(Duration::from_secs(30), terminal.wait())
        .await
        .expect("explicit stop retires the exact descendant");
    let cleanup = terminal
        .cleanup()
        .expect("retirement retains cleanup evidence");
    assert_eq!(cleanup.actor(), actor);
    assert!(cleanup.is_confirmed(), "{cleanup:?}");
}

#[tokio::test]
#[ignore = "requires production hosted compiler and prepared runtime inputs"]
async fn production_harness_recursive_captured_helper_and_typed_replies() {
    let files = tempfile::TempDir::new().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let authored_settings = settings.clone();
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_prepared_configured(&settings, &provider, |config| {
        config.model = "gpt-6.1-sol".into();
        crate::exomonad::write_fixture_project_config(
            &config.workspace.join(".exomonad"),
            "gpt-6.1-sol",
            |project| {
                project.launch.embedded = Some(authored_settings);
                project.models.insert("luna".into(), "gpt-6-luna".into());
                project.defaults.effort = crate::exomonad::ExomonadEffort::Low;
            },
        );
        commit_workspace(&config.workspace);
    })
    .await
    .expect("production preparation and host startup succeed");
    host.run_scenario(|host| {
        Box::pin(async move {
            let root_actor = host.context.actor.identity();
            let root_path = AgentPath("/root".into());
            let mut pending = VecDeque::new();
            let frozen = host.context.config.workspace_inputs.as_ref().unwrap();
            let completed = frozen.completed_entry_selections().unwrap();
            let coverage = frozen.prepared_toolset_coverage().unwrap();
            assert_eq!(coverage.len(), 1);
            let installation = host.context.observer.installation(root_actor).await;
            let Some(exomonad_actor::ToolsetAcquisition::DeploymentOriginal { recipe, original }) =
                installation.acquisition.as_ref()
            else {
                panic!("root installation must use its completed deployment original")
            };
            assert_eq!(
                (recipe, original),
                (&coverage[0].recipe, &coverage[0].original)
            );
            assert_eq!(completed.get(recipe), Some(original));
            admit_http_input(host).await;
            let mut root = next(host, &mut requests, &mut pending, &root_path).await;
            assert_eq!(root.request.model, "gpt-6.1-sol");
            root.call("recursive-setup", SETUP);
            root = next(host, &mut requests, &mut pending, &root_path).await;
            root.assert_value("recursive-setup", "True");
            let root_prefix = transcript(&root);
            let root_operation = operation(host, &root, ROOT_CALL);
            root.async_call(ROOT_CALL, ROOT);
            let root_wait = next(host, &mut requests, &mut pending, &root_path).await;
            unsettled(&root_wait, ROOT_CALL);
            let (child_actor, child) =
                next_descendant(host, &mut requests, &mut pending, root_actor).await;
            captured(&child, &root_prefix, ROOT_CALL);
            let child_path = identity(&child).actor;
            let child_prefix = transcript(&child);
            let child_operation = operation(host, &child, CHILD_CALL);
            child.async_call(CHILD_CALL, CHILD);
            let child_wait = next(host, &mut requests, &mut pending, &child_path).await;
            unsettled(&child_wait, CHILD_CALL);
            let (grandchild_actor, mut grandchild) =
                next_descendant(host, &mut requests, &mut pending, child_actor).await;
            captured(&grandchild, &child_prefix, CHILD_CALL);
            let grandchild_path = identity(&grandchild).actor;
            let graph = host.context.forest.inspect_host_graph();
            assert_eq!(graph.len(), 3);
            assert!(graph.iter().all(|node| node.model_actor));
            let child_worktree = &graph
                .iter()
                .find(|node| node.actor == child_actor)
                .unwrap()
                .bound_worktree;
            let grandchild_worktree = &graph
                .iter()
                .find(|node| node.actor == grandchild_actor)
                .unwrap()
                .bound_worktree;
            assert!(child_worktree.is_some());
            assert!(grandchild_worktree.is_some());
            assert_ne!(
                child_worktree, grandchild_worktree,
                "distinct actual fork worktrees"
            );
            let child_installation = host.context.observer.installation(child_actor).await;
            parked(host, &host.context.actor, &root_operation).await;
            parked(host, &child_installation.actor, &child_operation).await;
            yield_pending(host, root_wait, "recursive-root-yield", &root_operation).await;
            yield_pending(host, child_wait, "recursive-child-yield", &child_operation).await;

            grandchild.call("recursive-leaf-refusal", REFUSAL);
            grandchild = next(host, &mut requests, &mut pending, &grandchild_path).await;
            require_ghc_compile_rejection(
                &grandchild.settled_output("recursive-leaf-refusal"),
                &["AgentLaunch"],
            )
            .expect("leaf's narrow effect row refuses checkpoint before any launch");
            assert_eq!(host.context.forest.inspect_host_graph().len(), 3);
            pending_claim(host, &root_operation);
            pending_claim(host, &child_operation);
            grandchild.call("recursive-leaf-reply", GRANDCHILD_REPLY);
            grandchild = next(host, &mut requests, &mut pending, &grandchild_path).await;
            replied(&grandchild, "recursive-leaf-reply");
            grandchild.finish();

            let mut child = next(host, &mut requests, &mut pending, &child_path).await;
            child.assert_value(CHILD_CALL, "True");
            pending_claim(host, &root_operation);
            child.call("recursive-child-reply", CHILD_REPLY);
            child = next(host, &mut requests, &mut pending, &child_path).await;
            replied(&child, "recursive-child-reply");
            assert_eq!(
                explicit_display_text(&child.settled_output("recursive-child-reply")),
                "True"
            );
            stopped(host, grandchild_actor, child_actor).await;
            child.finish();

            root = next(host, &mut requests, &mut pending, &root_path).await;
            root.assert_value(ROOT_CALL, "True");
            assert_eq!(root.request.model, "gpt-6.1-sol");
            root.call("recursive-cleanup", CLEANUP);
            root = next(host, &mut requests, &mut pending, &root_path).await;
            root.assert_value("recursive-cleanup", "True");
            stopped(host, child_actor, root_actor).await;
            root.finish();
        })
    })
    .await;
}
