//! Real recursive model actors and native captures; only provider replies are scripted.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    commit_workspace, explicit_display_text, hosted_script_provider_with_envelopes,
    hosted_test_settings, next_hosted_script_round, require_ghc_compile_rejection,
    HostedScriptRound, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
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
const FRESH_REQUEST: &str = include_str!("fixtures/m3_fresh_default_request.hs");
const FRESH_CLEANUP: &str = include_str!("fixtures/m3_fresh_default_cleanup.hs");
const ROOT_CALL: &str = "recursive-root-await";
const CHILD_CALL: &str = "recursive-child-await";

fn identity(round: &HostedScriptRound) -> HostIdentity {
    round.host_identity()
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
    context_parent: Option<ActorRef>,
    parent_operation: &OperationId,
) -> (ActorRef, HostedScriptRound) {
    pending_claim(host, parent_operation);
    let round = match pending.pop_front() {
        Some(round) => round,
        None => host
            .context
            .while_root_live(
                "recursive descendant provider barrier",
                tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                    let mut poll = tokio::time::interval(Duration::from_millis(10));
                    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            round = requests.recv() => break round,
                            _ = poll.tick() => {
                                pending_claim(host, parent_operation);
                                for node in host.context.forest.inspect_host_graph() {
                                    if node.creator == Some(parent) && node.context_parent == context_parent {
                                        let installation = host.context.observer.installation(node.actor).await;
                                        if let Some(terminal) = installation.actor.terminal().get() {
                                            panic!("captured descendant {} retired before its provider turn: {:?}: {}",
                                                node.actor, terminal.kind, terminal.summary);
                                        }
                                    }
                                }
                            },
                        }
                    }
                }),
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
    assert_eq!(node.context_parent, context_parent);
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
    let text = serde_json::to_string(&retained).unwrap();
    let cross_model = prefix
        .iter()
        .any(|item| item.0["encrypted_content"] == "synthetic-opaque-gpt-6.1-sol");
    if cross_model {
        assert_eq!(round.request.model, "gpt-6-luna");
        let parent_users = prefix
            .iter()
            .filter(|item| item.0["role"] == "user")
            .collect::<Vec<_>>();
        let child_users = retained
            .iter()
            .filter(|item| item.0["role"] == "user")
            .collect::<Vec<_>>();
        assert!(
            child_users.starts_with(&parent_users),
            "original user history survives capture"
        );
        assert!(text.contains("Store-generated model portability note"));
        assert!(text.contains("Source origin/hash"));
        assert!(text.contains("visible-provider-history-gpt-6.1-sol"));
        assert!(!text.contains("synthetic-opaque-gpt-6.1-sol"));
        assert!(!text.contains("encrypted_content"));
        assert!(!text.contains("logprobs"));
    } else {
        assert!(
            retained.starts_with(prefix),
            "same-model native parent prefix survives capture"
        );
    }
    assert!(
        !text.contains(excluded_call),
        "BeforeCall excludes the unfinished invocation"
    );
    assert!(text.contains("recursive-setup"));
    assert!(text.contains("recursiveSeed"));
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
                let mut poll = tokio::time::interval(Duration::from_millis(10));
                poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    poll.tick().await;
                    pending_claim(host, operation);
                    if actor.hosted_workbench_waiting(&context).is_some() {
                        break;
                    }
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
    let yield_operation = OperationId {
        origin: round.origin(),
        request: request.clone(),
        call: CallId(call.into()),
    };
    round.function(call, "yield", json!({"until": null}));
    host.context
        .while_root_live(
            "recursive Engine yield is persisted",
            tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                let mut poll = tokio::time::interval(Duration::from_millis(10));
                poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    poll.tick().await;
                    pending_claim(host, operation);
                    let claims = host.runtime.store().claims(&yield_operation.call).unwrap();
                    if !claims
                        .iter()
                        .any(|claim| claim.operation == yield_operation && claim.request == request)
                    {
                        continue;
                    }
                    pending_claim(host, &yield_operation);
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
                }
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"))
        .expect("yield is recorded before a scripted descendant response is released");
}

fn issuing_request(host: &HostedTestRuntime, round: &HostedScriptRound) -> RequestId {
    host.runtime
        .store()
        .embedded_round_frontier(&identity(round))
        .unwrap()
        .pending_head
        .expect("provider request has its issuing frontier")
}

fn worktree_registry(host: &HostedTestRuntime) -> exomonad_worktree::WorktreeRegistry {
    let root = actor_worktree_storage_root(
        &host.context.config.workspace,
        host.context.config.run_directory.path(),
    )
    .unwrap();
    let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(root).unwrap();
    exomonad_worktree::WorktreeRegistry::open(&anchor, "registry").unwrap()
}

fn workspace_provenance(host: &HostedTestRuntime, child: &str, grandchild: &str) {
    let registry = worktree_registry(host);
    let child_id = exomonad_worktree::WorktreeId::from_raw(child.to_owned());
    let child = registry.get(&child_id).unwrap().unwrap();
    let grandchild = registry
        .get(&exomonad_worktree::WorktreeId::from_raw(
            grandchild.to_owned(),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        child.origin,
        exomonad_worktree::WorktreeOrigin::CurrentRepository
    );
    assert_eq!(
        child.source_repository,
        std::fs::canonicalize(&host.context.config.workspace).unwrap()
    );
    assert_eq!(
        grandchild.origin,
        exomonad_worktree::WorktreeOrigin::Worktree(child_id)
    );
    assert_eq!(grandchild.source_repository, child.cwd);
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

async fn command_cleanup(host: &HostedTestRuntime, actor: ActorRef) {
    host.context
        .config
        .command_resources
        .as_ref()
        .unwrap()
        .seal_producer(&format!("{}-{}", actor.id.0, actor.incarnation.0))
        .await
        .expect("the exact actor has no active commands or retained resource allocations");
}

fn prepared_default_sol_workspace(config: &mut ActorHostConfig) {
    super::scaffold_admission_tests::prepared_scaffold(config);
    let owner = exomonad_node::command_resources::CommandResources::delegated(
        exomonad_node::command_resources::CommandResourcePolicy {
            general_bytes: 512 * 1024 * 1024,
            protected_bytes: 0,
            swap_max_bytes: 0,
            ..Default::default()
        },
    )
    .expect("the isolated test cgroup delegates production command resources");
    config.command_resources =
        Some(exomonad_node::command_resources::CommandResourceClient::local(owner));
    config.model = "gpt-6.1-sol".into();
    config.jev = Some(std::sync::Arc::new(super::test_campaign::FixtureJev));
    crate::exomonad::edit_fixture_project_config(&config.workspace.join(".exomonad"), |project| {
        project.defaults.model = "gpt-6.1-sol".into();
        project.models.insert("luna".into(), "gpt-6-luna".into());
        project.defaults.effort = crate::exomonad::ExomonadEffort::Low;
    });
    commit_workspace(&config.workspace);
}

#[tokio::test]
#[ignore = "requires production hosted compiler and prepared runtime inputs"]
async fn production_harness_recursive_captured_helper_and_typed_replies() {
    let files = tempfile::TempDir::new().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let (provider, mut requests) = hosted_script_provider_with_envelopes();
    let host = HostedTestRuntime::start_prepared_configured(&settings, &provider, |config| {
        prepared_default_sol_workspace(config);
    })
    .await
    .expect("production preparation and host startup succeed");
    host.run_scenario(|host| {
        Box::pin(async move {
            host.assert_fresh_prepared_workspace_original().await;
            let root_actor = host.context.actor.identity();
            let root_path = AgentPath("/root".into());
            let mut pending = VecDeque::new();
            let frozen = host.context.config.workspace_inputs.as_ref().unwrap();
            let coverage = frozen.prepared_toolset_coverage().unwrap();
            assert_eq!(coverage.len(), 1);
            let installation = host.context.observer.installation(root_actor).await;
            assert_eq!(
                installation
                    .acquisition
                    .as_ref()
                    .and_then(exomonad_actor::ToolsetAcquisition::selection),
                Some(coverage[0].program.clone()),
                "root installs its exact prepared program"
            );
            host.http_input("Run the recursive captured-helper scenario.")
                .await
                .unwrap();
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
            let (child_actor, mut child) = next_descendant(
                host,
                &mut requests,
                &mut pending,
                root_actor,
                Some(root_actor),
                &root_operation,
            )
            .await;
            captured(&child, &root_prefix, ROOT_CALL);
            assert_eq!(transcript(&root_wait)[..root_prefix.len()], root_prefix);
            let native_turns = host
                .runtime
                .store()
                .replay_turns(&root_operation.request)
                .unwrap();
            assert!(
                native_turns
                    .iter()
                    .flat_map(|turn| &turn.model_response.items)
                    .any(|item| item.0["encrypted_content"] == "synthetic-opaque-gpt-6.1-sol"),
                "portability leaves the original native response envelope in Store"
            );
            let child_path = identity(&child).actor;
            child.function(
                "recursive-default-bash",
                "bash",
                json!({
                    "cmd": "printf captured-default-shell", "workdir": null,
                    "environment": null, "memory_mib": null, "tty": null, "stdin": null,
                    "yield_time_ms": 30000, "max_output_bytes": 2048,
                    "intent": "exercise the shipped workspace shell before the typed reply",
                }),
            );
            child = next(host, &mut requests, &mut pending, &child_path).await;
            let shell = child.settled_output("recursive-default-bash");
            assert_eq!(shell["status"], "committed", "{shell}");
            assert_eq!(shell["value"]["successful"], true, "{shell}");
            assert!(
                shell.to_string().contains("captured-default-shell"),
                "{shell}"
            );
            let child_prefix = transcript(&child);
            let child_operation = operation(host, &child, CHILD_CALL);
            child.async_call(CHILD_CALL, CHILD);
            let child_wait = next(host, &mut requests, &mut pending, &child_path).await;
            unsettled(&child_wait, CHILD_CALL);
            let (grandchild_actor, mut grandchild) = next_descendant(
                host,
                &mut requests,
                &mut pending,
                child_actor,
                Some(child_actor),
                &child_operation,
            )
            .await;
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
            workspace_provenance(
                host,
                child_worktree.as_deref().unwrap(),
                grandchild_worktree.as_deref().unwrap(),
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
            assert_eq!(
                child.settled_output("recursive-child-yield"),
                json!({"reason": "tool_result", "ready_results": [&child_operation]})
            );
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
            assert_eq!(
                root.settled_output("recursive-root-yield"),
                json!({"reason": "tool_result", "ready_results": [&root_operation]})
            );
            root.assert_value(ROOT_CALL, "True");
            assert_eq!(root.request.model, "gpt-6.1-sol");
            root.call("recursive-cleanup", CLEANUP);
            root = next(host, &mut requests, &mut pending, &root_path).await;
            root.assert_value("recursive-cleanup", "True");
            stopped(host, child_actor, root_actor).await;
            command_cleanup(host, child_actor).await;
            root.finish();
        })
    })
    .await;
}

/// FreshCtx exercises the shipped original toolset without model portability.
#[tokio::test]
#[ignore = "requires production hosted compiler and prepared runtime inputs"]
async fn production_harness_fresh_default_workspace_shell_and_typed_text_reply() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 2);
    let (provider, mut requests) = hosted_script_provider_with_envelopes();
    let host = HostedTestRuntime::start_prepared_configured(&settings, &provider, |config| {
        prepared_default_sol_workspace(config)
    })
    .await
    .expect("the shipped workspace prepares its exact original");
    host.run_scenario(|host| { Box::pin(async move {
        host.assert_fresh_prepared_workspace_original().await;
        host.http_input("Exercise a fresh full-default child and its typed Text reply.").await.unwrap();
        let root_actor = host.context.actor.identity();
        let root_path = AgentPath("/root".into());
        let mut pending = VecDeque::new();
        let root = next(host, &mut requests, &mut pending, &root_path).await;
        assert_eq!(root.request.model, "gpt-6.1-sol");
        let operation = operation(host, &root, "fresh-default-request");
        root.async_call("fresh-default-request", FRESH_REQUEST);
        let root_wait = next(host, &mut requests, &mut pending, &root_path).await;
        unsettled(&root_wait, "fresh-default-request");
        let (child_actor, child) = next_descendant(
            host, &mut requests, &mut pending, root_actor, None, &operation,
        ).await;
        let child_path = identity(&child).actor;
        assert!(!serde_json::to_string(&child.request.input).unwrap()
            .contains("visible-provider-history-gpt-6.1-sol"), "FreshCtx excludes parent history");
        let installation = host.context.observer.installation(child_actor).await;
        assert!(!installation.checkpoint);
        for name in ["bash", "haskell_sync", "haskell", "lookup", "submit_review"] {
            assert!(installation.tools.iter().any(|tool| tool.name() == name),
                "the actual default workspace installs {name}");
        }
        let graph = host.context.forest.inspect_host_graph();
        let child_node = graph.iter().find(|node| node.actor == child_actor).unwrap();
        assert!(child_node.bound_worktree.is_some());
        let worktree = worktree_registry(host).get(&exomonad_worktree::WorktreeId::from_raw(
            child_node.bound_worktree.as_ref().unwrap().clone(),
        )).unwrap().unwrap();
        assert_eq!(worktree.origin, exomonad_worktree::WorktreeOrigin::CurrentRepository);
        assert_ne!(worktree.cwd, host.context.config.workspace);
        assert_eq!(worktree.source_repository, std::fs::canonicalize(&host.context.config.workspace).unwrap());
        yield_pending(host, root_wait, "fresh-default-yield", &operation).await;
        let terminal = installation.actor.terminal();
        child.call("fresh-default-input", "let inputText = sessionInput :: Text\ndisplay (inputText == \"return fresh-default-native-reply\")");
        let child = tokio::select! {
            biased;
            terminal = terminal.wait() => panic!("fresh default child retired while binding its request input: {:?}: {}", terminal.kind, terminal.summary),
            child = next(host, &mut requests, &mut pending, &child_path) => child,
        };
        child.assert_value("fresh-default-input", "True");
        child.function("fresh-default-bash", "bash", json!({
            "cmd": "printf fresh-default-shell", "workdir": null, "environment": null,
            "memory_mib": null, "tty": null, "stdin": null, "yield_time_ms": 30000,
            "max_output_bytes": 2048, "intent": "execute the full default workspace command closure",
        }));
        let child = tokio::select! {
            biased;
            terminal = terminal.wait() => panic!("fresh default child retired after bash: {:?}: {}", terminal.kind, terminal.summary),
            child = next(host, &mut requests, &mut pending, &child_path) => child,
        };
        let shell = child.settled_output("fresh-default-bash");
        assert_eq!(shell["status"], "committed", "{shell}");
        assert_eq!(shell["value"]["successful"], true, "{shell}");
        assert!(shell.to_string().contains("fresh-default-shell"), "{shell}");
        child.call("fresh-default-reply", "respond (\"fresh-default-native-reply\" :: Text)");
        let child = tokio::select! {
            biased;
            terminal = terminal.wait() => panic!("fresh default child retired before its typed reply: {:?}: {}", terminal.kind, terminal.summary),
            child = next(host, &mut requests, &mut pending, &child_path) => child,
        };
        replied(&child, "fresh-default-reply");
        child.finish();
        let root = next(host, &mut requests, &mut pending, &root_path).await;
        root.assert_value("fresh-default-request", "True");
        assert_eq!(root.settled_output("fresh-default-yield"),
            json!({"reason": "tool_result", "ready_results": [&operation]}));
        root.call("fresh-default-cleanup", FRESH_CLEANUP);
        let root = next(host, &mut requests, &mut pending, &root_path).await;
        root.assert_value("fresh-default-cleanup", "True");
        let terminal = installation.actor.terminal();
        tokio::time::timeout(Duration::from_secs(30), terminal.wait()).await.unwrap();
        assert!(terminal.cleanup().unwrap().is_confirmed());
        command_cleanup(host, child_actor).await;
        root.finish();
    }) }).await;
}
