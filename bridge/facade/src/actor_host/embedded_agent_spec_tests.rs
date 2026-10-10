//! Retained installed tools and their after-tool slot survive a live spec reload.

use super::hosted_test_context::{HostedActorContext, HostedTestRuntime, ObservedInstallation};
use super::test_campaign::{commit_workspace, dispatch_structured_tool, TestCampaign};
use super::test_campaign::{
    hosted_script_provider, hosted_test_settings, next_hosted_script_round,
    COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
};
use std::sync::Arc;
use std::time::Duration;

struct HeldSuppliedCall {
    request: String,
    release: tokio::sync::oneshot::Sender<()>,
}

/// The existing Jev interpreter supplies the admission barrier. The test
/// releases the actual suspended handler, rather than an endpoint wrapper.
struct SuppliedCallGate(tokio::sync::mpsc::UnboundedSender<HeldSuppliedCall>);

impl exomonad_actor::JevBackend for SuppliedCallGate {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, exomonad_actor::JevCallFailure>> {
        Box::pin(async move {
            let (release, held) = tokio::sync::oneshot::channel();
            self.0
                .send(HeldSuppliedCall { request, release })
                .map_err(|_| {
                    exomonad_actor::JevCallFailure::JevTransport(
                        "test admission observer closed".into(),
                    )
                })?;
            held.await.map_err(|_| {
                exomonad_actor::JevCallFailure::JevTransport(
                    "test abandoned admitted handler".into(),
                )
            })?;
            Ok("{}".into())
        })
    }
}

fn configure_supplied_spec(config: &mut super::ActorHostConfig) {
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    std::fs::write(
        authored.join("Project/Supplied.hs"),
        include_str!("fixtures/supplied_live_tools.hs"),
    )
    .unwrap();
    std::fs::write(
        authored.join("AgentSpec.hs"),
        include_str!("fixtures/supplied_live_root.hs"),
    )
    .unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec!["Project.Supplied".into(), "AgentSpec".into()];
        project.haskell.spec = Some("AgentSpec.agentSpec".into());
    });
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .unwrap(),
    );
}

async fn supplied_children(context: &HostedActorContext) -> Vec<ObservedInstallation> {
    tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        let mut children = Vec::new();
        for label in ["supplied-live-first", "supplied-live-second"] {
            let node = context
                .forest
                .inspect_host_graph()
                .into_iter()
                .find(|node| node.label == label)
                .expect("committed spawn has an exact actor record");
            assert_eq!(node.creator, Some(context.actor.identity()));
            assert_eq!(node.context_parent, None, "FreshCtx selects fresh context");
            assert!(node.terminal.is_none(), "{node:?}");
            assert!(node.active_requests.is_empty(), "idle spawn: {node:?}");
            assert!(node.queued_requests.is_empty(), "idle spawn: {node:?}");
            let installation = context.observer.installation(node.actor).await;
            assert!(installation
                .tools
                .iter()
                .any(|tool| tool.name() == "distinctive"));
            assert!(context
                .binding(node.actor)
                .and_then(|binding| binding.conversation())
                .is_some());
            children.push(installation);
        }
        assert_ne!(children[0].actor.identity(), children[1].actor.identity());
        let graph = context.forest.inspect_host_graph();
        let workspace = |actor| {
            graph
                .iter()
                .find(|node| node.actor == actor)
                .unwrap()
                .bound_worktree
                .as_ref()
                .unwrap()
        };
        assert_eq!(
            workspace(children[0].actor.identity()),
            workspace(children[1].actor.identity())
        );
        children
    })
    .await
    .expect("supplied idle installations and attachments are observable")
}

fn child_path(
    context: &HostedActorContext,
    actor: exomonad_actor::ActorRef,
) -> harness::model::AgentPath {
    context
        .binding(actor)
        .unwrap()
        .conversation()
        .unwrap()
        .identity()
        .actor
        .clone()
}

fn assert_supplied_answer(receipt: &serde_json::Value, expected: &str) {
    assert!(
        matches!(receipt["status"].as_str(), Some("committed" | "completed")),
        "{receipt}"
    );
    let [item] = receipt["items"].as_array().unwrap().as_slice() else {
        panic!("typed handler retains one receipt: {receipt}");
    };
    assert_eq!(item["status"], "committed", "{receipt}");
    assert_eq!(
        item["output"].as_str().unwrap().trim(),
        expected,
        "{receipt}"
    );
}

async fn supplied_probe(
    snapshot: &dyn exomonad_actor::ResidentToolEndpoint,
    held: bool,
) -> serde_json::Value {
    tokio::time::timeout(
        COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
        dispatch_structured_tool(
            snapshot,
            "distinctive",
            serde_json::json!({"number":10,"hold":held}),
        ),
    )
    .await
    .expect("supplied handler dispatch bounded")
}

/// The counted runner executes this test alone in its process. Reuse the
/// existing JSON tracing subscriber, retaining only this campaign's suffix.
struct InstallerTrace {
    path: std::path::PathBuf,
    before: usize,
    selected_here: bool,
}

impl InstallerTrace {
    fn select(files: &tempfile::TempDir) -> Self {
        let prior = std::env::var_os("TIDEPOOL_TEST_TRACE");
        let path = prior
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| files.path().join("supplied-installer-trace.jsonl"));
        let before = std::fs::metadata(&path)
            .map(|metadata| metadata.len() as usize)
            .unwrap_or(0);
        if prior.is_none() {
            std::env::set_var("TIDEPOOL_TEST_TRACE", &path);
        }
        Self {
            path,
            before,
            selected_here: prior.is_none(),
        }
    }

    fn assert_two_compiled_installs(&self, children: &[ObservedInstallation]) {
        let bytes = std::fs::read(&self.path).expect("existing compiler and installer phase trace");
        let suffix = std::str::from_utf8(&bytes[self.before..]).unwrap();
        let events: Vec<serde_json::Value> = suffix
            .lines()
            .map(|line| serde_json::from_str(line).expect("complete JSON trace row"))
            .collect();
        let mut phases = std::collections::BTreeMap::new();
        let mut compiler_observed = false;
        for event in &events {
            let current = &event["span"];
            let parents = event["spans"].as_array().into_iter().flatten();
            let explicit = parents.clone().chain(std::iter::once(current)).any(|span| {
                span["name"] == "compiled_spec_install" && span["origin"] == "explicit_live"
            });
            if current["name"] == "compile_request" {
                compiler_observed = true;
                assert!(
                    !explicit,
                    "an explicit compiled installer submitted compiler work: {event}"
                );
            }
            if current["name"] == "compiled_spec_install" && event["fields"]["message"] == "new" {
                assert_eq!(current["origin"], "explicit_live");
                assert!(
                    current["installation"].as_u64().is_some(),
                    "exact installation correlation: {event}"
                );
                *phases
                    .entry(current["actor"].as_str().unwrap().to_owned())
                    .or_insert(0) += 1;
            }
        }
        assert!(
            compiler_observed,
            "the trace must observe real parent/bootstrap compiler requests"
        );
        assert_eq!(phases.len(), 2, "{phases:?}");
        for child in children {
            assert_eq!(
                phases.get(&child.actor.identity().to_string()),
                Some(&1),
                "one compiled installer execution per exact child: {phases:?}"
            );
        }
    }
}

impl Drop for InstallerTrace {
    fn drop(&mut self) {
        if self.selected_here {
            std::env::remove_var("TIDEPOOL_TEST_TRACE");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn supplied_spec_fresh_existing_workspace_is_idle_until_separate_typed_request() {
    let files = tempfile::tempdir().unwrap();
    let trace = InstallerTrace::select(&files);
    let settings = hosted_test_settings(&files, 3);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, _admitted) = tokio::sync::mpsc::unbounded_channel();
    let host = HostedTestRuntime::start_configured(&settings, &provider, move |config| {
        configure_supplied_spec(config);
        config.jev = Some(Arc::new(SuppliedCallGate(entered)));
    })
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Install supplied specs before making a separate typed request.")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = std::collections::VecDeque::new();
            let root_round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            assert!(!root_round
                .request
                .tools
                .iter()
                .any(|tool| tool["name"] == "distinctive"));
            root_round.call(
                "supplied-idle-spawn",
                include_str!("fixtures/supplied_live_spawn.hs"),
            );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("supplied-idle-spawn", "True");
            let children = supplied_children(&host.context).await;
            assert!(
                pending.is_empty(),
                "idle fresh children cannot request inference"
            );
            assert!(matches!(
                requests.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
            trace.assert_two_compiled_installs(&children);
            let child = child_path(&host.context, children[0].actor.identity());
            root_after.call(
                "supplied-first-request",
                include_str!("fixtures/supplied_live_request.hs"),
            );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("supplied-first-request", "True");
            let child_round = next_hosted_script_round(&mut requests, &mut pending, &child).await;
            let first_session = child_round.request.session_id.clone();
            assert!(child_round
                .request
                .tools
                .iter()
                .any(|tool| tool["name"] == "distinctive"));
            let before_call = tidepool_extract_cmd::extract_spawn_count();
            child_round.function(
                "supplied-distinctive-call",
                "distinctive",
                serde_json::json!({"number":10,"hold":false}),
            );
            let child_after = next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert_supplied_answer(
                &child_after.settled_output("supplied-distinctive-call"),
                "111",
            );
            assert_eq!(
                tidepool_extract_cmd::extract_spawn_count(),
                before_call,
                "first typed handler uses its installed code"
            );
            child_after.call(
                "supplied-typed-response",
                "respond (sessionInput + 101 :: Int)",
            );
            let child_after = next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert_eq!(
                child_after.settled_output("supplied-typed-response")["status"],
                "replied"
            );
            child_after.finish();
            root_after.call(
                "supplied-observe-result",
                include_str!("fixtures/supplied_live_observe.hs"),
            );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("supplied-observe-result", "True");
            root_after.call(
                "supplied-second-request",
                include_str!("fixtures/supplied_live_request_again.hs"),
            );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("supplied-second-request", "True");
            let second_child_round =
                next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert_eq!(second_child_round.request.session_id, first_session);
            assert!(second_child_round
                .request
                .tools
                .iter()
                .any(|tool| tool["name"] == "distinctive"));
            second_child_round.function(
                "supplied-second-distinctive-call",
                "distinctive",
                serde_json::json!({"number":20,"hold":false}),
            );
            let second_child_after =
                next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert_supplied_answer(
                &second_child_after.settled_output("supplied-second-distinctive-call"),
                "121",
            );
            second_child_after.call(
                "supplied-second-typed-response",
                "respond (sessionInput + 101 :: Int)",
            );
            let second_child_after =
                next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert_eq!(
                second_child_after.settled_output("supplied-second-typed-response")["status"],
                "replied"
            );
            second_child_after.finish();
            root_after.call(
                "supplied-observe-second-result",
                include_str!("fixtures/supplied_live_observe_again.hs"),
            );
            let root_done = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_done.assert_value("supplied-observe-second-result", "True");
            root_done.finish();
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn supplied_spec_request_queued_in_spawn_cell_reaches_installed_receiver() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        configure_supplied_spec(config);
    })
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Spawn a supplied spec and immediately send it typed work.")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = std::collections::VecDeque::new();
            let root_round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_round.call(
                "supplied-immediate-spawn-and-request",
                include_str!("fixtures/supplied_live_spawn_request_same_cell.hs"),
            );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("supplied-immediate-spawn-and-request", "True");

            let child_node = host
                .context
                .forest
                .inspect_host_graph()
                .into_iter()
                .find(|node| node.label == "supplied-live-immediate")
                .expect("same-cell spawn commits its supplied child");
            assert_eq!(child_node.creator, Some(host.context.actor.identity()));
            assert_eq!(
                child_node.context_parent, None,
                "FreshCtx selects fresh context"
            );
            let child = child_path(&host.context, child_node.actor);
            let child_round = next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert!(child_round
                .request
                .tools
                .iter()
                .any(|tool| tool["name"] == "distinctive"));
            let before_call = tidepool_extract_cmd::extract_spawn_count();
            child_round.function(
                "supplied-immediate-distinctive-call",
                "distinctive",
                serde_json::json!({"number":30,"hold":false}),
            );
            let child_after = next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert_supplied_answer(
                &child_after.settled_output("supplied-immediate-distinctive-call"),
                "131",
            );
            assert_eq!(
                tidepool_extract_cmd::extract_spawn_count(),
                before_call,
                "immediate typed handler uses its installed code"
            );
            child_after.call(
                "supplied-immediate-typed-response",
                "respond (sessionInput + 101 :: Int)",
            );
            let child_after = next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert_eq!(
                child_after.settled_output("supplied-immediate-typed-response")["status"],
                "replied"
            );
            child_after.finish();
            root_after.call(
                "supplied-observe-immediate-result",
                include_str!("fixtures/supplied_live_observe_immediate.hs"),
            );
            let root_done = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_done.assert_value("supplied-observe-immediate-result", "True");
            root_done.finish();
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the native hosted provider and compiler acceptance resources"]
async fn accepted_reply_resumes_after_parked_notebook_and_provider_completion() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 4);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut admitted) = tokio::sync::mpsc::unbounded_channel();
    let host = HostedTestRuntime::start_configured(&settings, &provider, move |config| {
        config.jev = Some(Arc::new(SuppliedCallGate(entered)));
    })
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Reply while a notebook task remains parked.")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = std::collections::VecDeque::new();
            next_hosted_script_round(&mut requests, &mut pending, &root)
                .await
                .call(
                    "deferred-spawn",
                    include_str!("fixtures/deferred_reply_spawn.hs"),
                );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("deferred-spawn", "True");
            let child_node = host
                .context
                .forest
                .inspect_host_graph()
                .into_iter()
                .find(|node| node.label == "deferred-reply-child")
                .unwrap();
            let child = child_path(&host.context, child_node.actor);
            next_hosted_script_round(&mut requests, &mut pending, &child)
                .await
                .async_call(
                    "deferred-sibling",
                    include_str!("fixtures/deferred_reply_sibling.hs"),
                );
            let held = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, admitted.recv())
                .await
                .expect("actual notebook reaches Jev gate")
                .unwrap();
            assert_eq!(held.request, "deferred-reply-sibling");
            next_hosted_script_round(&mut requests, &mut pending, &child).await.async_call(
                "deferred-late-cell", include_str!("fixtures/deferred_reply_late.hs"));
            let late = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, admitted.recv())
                .await.expect("second actual notebook is admitted before reply").unwrap();
            assert_eq!(late.request, "deferred-reply-late");
            let parent_await = root_after.operation("deferred-parent-await");
            root_after.call(
                "deferred-parent-await",
                include_str!("fixtures/deferred_reply_await.hs"),
            );
            host.while_operation_succeeds(&parent_await, tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                loop {
                    let parent = host.context.forest.inspect_host_graph().into_iter()
                        .find(|node| node.actor == host.context.actor.identity()).unwrap();
                    if matches!(parent.workbench, exomonad_actor::ActorWorkbenchPosture::AwaitingEffect { ref effect, .. }
                        if effect == "awaitWatch") {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })).await.unwrap_or_else(|error| panic!("parent typed child watch failed: {error}"))
                .expect("parent parks on its typed child watch before reply");
            next_hosted_script_round(&mut requests, &mut pending, &child)
                .await
                .async_call("deferred-child-reply", "respond (\"first accepted failure\" :: Text)");
            let child_after = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                let mut waits = 0;
                loop {
                    let round = next_hosted_script_round(&mut requests, &mut pending, &child).await;
                    if round.request.input.iter().any(|item|
                        item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == "deferred-child-reply") {
                        break round;
                    }
                    round.function(&format!("deferred-reply-yield-{waits}"), "yield", serde_json::json!({"until":1}));
                    waits += 1;
                }
            }).await.expect("actual async respond settles while other notebooks remain parked");
            let reply = child_after.settled_output("deferred-child-reply");
            assert_eq!(reply["status"], "replied", "{reply}");
            assert_eq!(
                reply["items"][0]["terminalTransfer"], "replyAccepted",
                "{reply}"
            );
            let node = host.context.forest.inspect_host_graph().into_iter()
                .find(|node| node.actor == child_node.actor).unwrap();
            assert!(!node.active_requests.is_empty(), "accepted reply still owns its continuation");
            // A second reply is a later invocation, not replacement authority
            // for the accepted value. It waits behind the owned continuation.
            child_after.async_call("deferred-second-reply", "respond (\"later success\" :: Text)");
            let child_final = next_hosted_script_round(&mut requests, &mut pending, &child).await;
            child_final.finish();
            held.release.send(()).unwrap();
            late.release.send(()).unwrap();
            // Late asynchronous outputs start another provider round even
            // after final prose. Consume their actual receipts before finishing
            // that round; provider prose carries no request settlement authority.
            let late_outputs = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                let mut waits = 0;
                loop {
                    let round = next_hosted_script_round(&mut requests, &mut pending, &child).await;
                    if ["deferred-sibling", "deferred-late-cell", "deferred-second-reply"].iter().all(|call|
                        round.request.input.iter().any(|item| item.0["type"] == "custom_tool_call_output"
                            && item.0["call_id"] == *call)) {
                        break round;
                    }
                    round.function(&format!("deferred-late-yield-{waits}"), "yield", serde_json::json!({"until":1}));
                    waits += 1;
                }
            }).await.expect("all late actual notebook outputs reach the provider");
            late_outputs.assert_committed("deferred-sibling");
            late_outputs.assert_committed("deferred-late-cell");
            assert_eq!(late_outputs.settled_output("deferred-second-reply")["status"], "rejected");
            late_outputs.finish();
            let root_done = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_done.assert_value("deferred-parent-await", "True");
            root_done.finish();
            tokio::time::timeout(Duration::from_secs(120), async {
                loop {
                    let node = host
                        .context
                        .forest
                        .inspect_host_graph()
                        .into_iter()
                        .find(|node| node.actor == child_node.actor)
                        .unwrap();
                    if node.provider_turn.is_some_and(|turn| {
                        turn.state == exomonad_model::ProviderTurnState::Succeeded
                    }) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("provider finishes after both actual late calls settle");
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn public_replace_spec_pins_admitted_handler_and_refuses_changed_surface() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, mut admitted) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new(SuppliedCallGate(entered));
    let host = HostedTestRuntime::start_configured(&settings, &provider, move |config| {
        configure_supplied_spec(config);
        config.jev = Some(gate);
    })
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Replace a supplied spec while its admitted handler remains held.")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = std::collections::VecDeque::new();
            next_hosted_script_round(&mut requests, &mut pending, &root)
                .await
                .call(
                    "replacement-idle-spawn",
                    include_str!("fixtures/supplied_live_spawn.hs"),
                );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("replacement-idle-spawn", "True");
            let children = supplied_children(&host.context).await;
            let current = children[0].policy.clone();
            let admitted_endpoint = current.snapshot_for_request().unwrap();
            let old_call =
                tokio::spawn(async move { supplied_probe(admitted_endpoint.as_ref(), true).await });
            let held = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, admitted.recv())
                .await
                .expect("actual installed handler reaches Jev admission")
                .unwrap();
            assert_eq!(held.request, "supplied-admitted-handler");
            assert!(!old_call.is_finished());
            root_after.call(
                "same-surface-replace",
                include_str!("fixtures/supplied_live_replace.hs"),
            );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("same-surface-replace", "True");
            assert!(
                !old_call.is_finished(),
                "replacement cannot settle the admitted old call"
            );
            let replaced_endpoint = current.snapshot_for_request().unwrap();
            let after_replace = tidepool_extract_cmd::extract_spawn_count();
            assert_supplied_answer(
                &supplied_probe(replaced_endpoint.as_ref(), false).await,
                "212",
            );
            held.release.send(()).unwrap();
            assert_supplied_answer(
                &tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, old_call)
                    .await
                    .expect("held old handler completes")
                    .unwrap(),
                "111",
            );
            assert_eq!(
                tidepool_extract_cmd::extract_spawn_count(),
                after_replace,
                "both retained generations execute without compiler work"
            );
            root_after.call(
                "changed-surface-refusal",
                include_str!("fixtures/supplied_live_refuse.hs"),
            );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("changed-surface-refusal", "True");
            let after_refusal = tidepool_extract_cmd::extract_spawn_count();
            let unchanged_endpoint = current.snapshot_for_request().unwrap();
            assert_supplied_answer(
                &supplied_probe(unchanged_endpoint.as_ref(), false).await,
                "212",
            );
            assert_supplied_answer(
                &supplied_probe(replaced_endpoint.as_ref(), false).await,
                "212",
            );
            assert_eq!(
                tidepool_extract_cmd::extract_spawn_count(),
                after_refusal,
                "refusal preserves the installed and retained handler"
            );
            root_after.finish();
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_child_model_alias_retains_failure_and_parent_can_request_valid_child() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let (provider, mut requests) = hosted_script_provider();
    let (entered, _admitted) = tokio::sync::mpsc::unbounded_channel();
    let host = HostedTestRuntime::start_configured(&settings, &provider, move |config| {
        configure_supplied_spec(config);
        config.jev = Some(Arc::new(SuppliedCallGate(entered)));
    })
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Refuse one child's model alias without retiring its parent.")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = std::collections::VecDeque::new();
            next_hosted_script_round(&mut requests, &mut pending, &root)
                .await
                .call(
                    "unknown-child-model",
                    include_str!("fixtures/child_attachment_alias_failure.hs"),
                );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("unknown-child-model", "True");
            let failed = host
                .context
                .forest
                .inspect_host_graph()
                .into_iter()
                .find(|node| node.label == "unknown-alias-child")
                .expect("typed failure retains its exact admitted child");
            assert_eq!(failed.creator, Some(host.context.actor.identity()));
            assert!(failed.active_requests.is_empty(), "{failed:?}");
            assert!(failed.queued_requests.is_empty(), "{failed:?}");
            let installation = tokio::time::timeout(
                COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                host.context.observer.installation(failed.actor),
            )
            .await
            .expect("attachment refusal occurs after actual child policy installation");
            let terminal = tokio::time::timeout(
                COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                installation.actor.terminal().wait(),
            )
            .await
            .expect("refused child retirement is observable");
            // Preserve the real cleanup observation; a failed attachment alone
            // does not certify that native custody has been released.
            eprintln!(
                "refused child {:?}: terminal={terminal:?}, cleanup={:?}",
                failed.actor,
                installation.actor.terminal().cleanup()
            );
            assert!(
                pending.is_empty(),
                "the refused child cannot activate inference"
            );
            assert!(matches!(
                requests.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
            root_after.call("parent-after-child-refusal", "display (6 * 7 :: Int)");
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("parent-after-child-refusal", "42");
            root_after.call("retained-child-failure", "display (show attachmentFailure)");
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_committed("retained-child-failure");
            let retained = root_after.settled_output("retained-child-failure");
            assert!(
                retained.to_string().contains("SpawnPartialFailure"),
                "{retained}"
            );
            assert!(
                retained.to_string().contains("SpawnRetainedActor"),
                "{retained}"
            );
            assert!(
                retained.to_string().contains("SpawnCleanup"),
                "the typed receipt retains its actual cleanup category: {retained}"
            );
            assert!(
                retained
                    .to_string()
                    .contains("deliberately-unknown-child-alias"),
                "{retained}"
            );
            root_after.call(
                "exact-retained-child",
                &format!(
                    "display (case attachmentFailure of {{ Left (SpawnPartialFailure (SpawnRetainedActor child) _ _) -> agentIdentity child == ({}, {}); _ -> False }})",
                    failed.actor.id.0, failed.actor.incarnation.0
                ),
            );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("exact-retained-child", "True");
            root_after.call(
                "valid-child-after-refusal",
                include_str!("fixtures/child_attachment_valid_request.hs"),
            );
            let root_after = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after.assert_value("valid-child-after-refusal", "True");
            let valid = host
                .context
                .forest
                .inspect_host_graph()
                .into_iter()
                .find(|node| node.label == "valid-after-alias-child")
                .expect("subsequent child is admitted and attached");
            assert_ne!(valid.actor, failed.actor);
            assert_eq!(valid.bound_worktree, failed.bound_worktree);
            tokio::time::timeout(
                COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                host.context.observer.installation(valid.actor),
            )
            .await
            .expect("subsequent child publishes its actual installed policy");
            let child = child_path(&host.context, valid.actor);
            next_hosted_script_round(&mut requests, &mut pending, &child)
                .await
                .call("reply-after-alias-refusal", "respond (111 :: Int)");
            let child_after = next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert_eq!(
                child_after.settled_output("reply-after-alias-refusal")["status"],
                "replied"
            );
            child_after.finish();
            root_after.call(
                "observe-reply-after-alias-refusal",
                include_str!("fixtures/child_attachment_observe_reply.hs"),
            );
            let root_done = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_done.assert_value("observe-reply-after-alias-refusal", "True");
            assert!(host.context.actor.terminal().get().is_none());
            root_done.finish();
        })
    })
    .await;
}

fn write_spec(workspace: &std::path::Path, handler: &str, slot: &str) {
    let authored = workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    std::fs::write(
        authored.join("Project/Tools.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/retained_handler_tools.hs",
        )
        .replace("HANDLER_GENERATION", handler),
    )
    .unwrap();
    std::fs::write(
        authored.join("AgentSpec.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/retained_handler_agent_spec.hs",
        )
        .replace("SLOT_GENERATION", slot),
    )
    .unwrap();
}

async fn probe(policy: &dyn exomonad_actor::ResidentToolEndpoint) -> String {
    dispatch_structured_tool(policy, "probe", serde_json::json!({"topic": "anything"}))
        .await
        .to_string()
}

#[tokio::test]
async fn issued_tool_snapshot_keeps_old_handler_after_spec_reload() {
    let before_install = tidepool_extract_cmd::extract_spawn_count();
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            write_spec(&config.workspace, "old-handler", "old slot");
            crate::exomonad::write_fixture_project_config(
                &config.workspace.join(".exomonad"),
                "test-model",
                |project| {
                    project.haskell.source_roots = vec![".".into()];
                    project.haskell.modules = vec!["Project.Tools".into(), "AgentSpec".into()];
                    project.haskell.spec = Some("AgentSpec.agentSpec".into());
                },
            );
            commit_workspace(&config.workspace);
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
                let after_install = tidepool_extract_cmd::extract_spawn_count();
                assert!(
                    after_install > before_install,
                    "the accepted AgentSpec installation must submit compiler work"
                );
                let workspace = campaign._repository.path().to_path_buf();
                let policy = campaign.root_installation.policy.clone();
                let old_request = policy
                    .snapshot_for_request()
                    .expect("initial installed spec");

                let first_old_result = probe(old_request.as_ref()).await;
                assert!(
                    first_old_result.contains("old-handler"),
                    "{first_old_result}"
                );
                assert!(first_old_result.contains("old slot"), "{first_old_result}");
                assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        after_install,
        "the first typed tool call must use the installed executable without compiler work"
    );
                let repeated_old_result = probe(old_request.as_ref()).await;
                assert!(
                    repeated_old_result.contains("old-handler"),
                    "{repeated_old_result}"
                );
                assert!(
                    repeated_old_result.contains("old slot"),
                    "{repeated_old_result}"
                );
                assert_eq!(
                    tidepool_extract_cmd::extract_spawn_count(),
                    after_install,
                    "repeated typed tool calls must keep using the installed executable"
                );

                write_spec(&workspace, "new-handler", "new slot");
                let before_reload = tidepool_extract_cmd::extract_spawn_count();
                let receipt = dispatch_structured_tool(
                    old_request.as_ref(),
                    "reload_agent_spec",
                    serde_json::json!({}),
                )
                .await
                .to_string();
                assert!(receipt.contains("swapped"), "{receipt}");
                let new_request = policy
                    .snapshot_for_request()
                    .expect("reloaded installed spec");
                let after_reload = tidepool_extract_cmd::extract_spawn_count();
                assert!(
        after_reload > before_reload,
        "same-surface reload must submit compiler work before publishing its dispatcher"
    );

                let old_result = probe(old_request.as_ref()).await;
                assert!(old_result.contains("old-handler"), "{old_result}");
                assert!(old_result.contains("old slot"), "{old_result}");
                assert!(!old_result.contains("new-handler"), "{old_result}");
                assert!(!old_result.contains("new slot"), "{old_result}");
                let new_result = probe(new_request.as_ref()).await;
                assert!(new_result.contains("new-handler"), "{new_result}");
                assert!(new_result.contains("new slot"), "{new_result}");
                assert!(!new_result.contains("old-handler"), "{new_result}");
                assert!(!new_result.contains("old slot"), "{new_result}");
                let repeated_new_result = probe(new_request.as_ref()).await;
                assert!(
                    repeated_new_result.contains("new-handler"),
                    "{repeated_new_result}"
                );
                assert!(
                    repeated_new_result.contains("new slot"),
                    "{repeated_new_result}"
                );
                assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        after_reload,
        "first and repeated calls through both retained generations must issue no compiler requests"
    );
            })
        })
        .await;
}
