//! Execute the exact published tutorial through real source capture, Harness,
//! installed Sync handlers, Web normalization and typed request settlement.
use super::hosted_test_context::{HostedChildMachines, HostedTestRuntime};
use super::test_campaign::{
    commit_workspace, hosted_script_provider, hosted_test_settings, next_hosted_child_round,
    next_hosted_script_round, select_hosted_script_round, HostedScriptRound, HostedScriptSelection,
};
use super::ResidentToolEndpointTestExt;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tidepool_handlers::{WebError, WebOperation, WebTextResponse, WebTransport};

const PAGE: &str = "https://example.org/moss";
const BODY: &str = "Moss retains water.\nIts leaves absorb moisture. λ\n";

#[derive(Default)]
struct ResearchWeb(Mutex<Vec<(WebOperation, Value)>>);
impl WebTransport for ResearchWeb {
    fn post(&self, operation: WebOperation, body: &Value) -> Result<Value, WebError> {
        self.0.lock().unwrap().push((operation, body.clone()));
        match operation {
            WebOperation::Search => {
                Ok(json!({"results":[{"title":"Moss","url":PAGE,"content":"Moss retains water."}]}))
            }
            WebOperation::Extract => Ok(json!({"results":[{"url":PAGE,"raw_content":BODY}]})),
        }
    }
    fn fetch_text(&self, _url: &str) -> Result<WebTextResponse, WebError> {
        Err(WebError::WebUnavailable(
            "this example uses the explicit extraction tool".into(),
        ))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResearchScript {
    Tutorial,
    RefusalControls,
}

fn configure_research(config: &mut super::ActorHostConfig, script: ResearchScript) {
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    std::fs::write(
        authored.join("Project/Researcher.hs"),
        tidepool_testing::fixture_source(
            "exomonad/examples/workspace/.exomonad/Project/Researcher.hs",
        ),
    )
    .unwrap();
    std::fs::write(
        authored.join("AgentSpec.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/researcher_agent_spec.hs",
        ),
    )
    .unwrap();
    if script == ResearchScript::RefusalControls {
        std::fs::write(
            authored.join("Project/PositiveReport.hs"),
            tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/fixtures/researcher_positive_report.hs",
            ),
        )
        .unwrap();
    }
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.models.insert("luna".into(), "gpt-6-luna".into());
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec!["Project.Researcher".into(), "AgentSpec".into()];
        if script == ResearchScript::RefusalControls {
            project
                .haskell
                .modules
                .push("Project.PositiveReport".into());
        }
        project.haskell.spec = Some("AgentSpec.agentSpec".into());
    });
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            config.run_directory.path(),
        )
        .unwrap(),
    );
}

fn reply_properties(round: &HostedScriptRound) -> Vec<String> {
    let reply = round
        .request
        .tools
        .iter()
        .find(|tool| tool["name"] == "reply")
        .expect("active typed reply endpoint");
    let properties = reply["parameters"]["properties"]["result"]["properties"]
        .as_object()
        .expect("typed result schema");
    let mut names = properties.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
}

async fn research_history(child_machines: HostedChildMachines, script: ResearchScript) {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 4);
    let (provider, mut requests) = hosted_script_provider();
    let web = Arc::new(ResearchWeb::default());
    let host = HostedTestRuntime::start_configured_with_web(
        &settings,
        &provider,
        web.clone(),
        child_machines,
        move |config| configure_research(config, script),
    )
    .await
    .unwrap();
    host.run_scenario(|host| Box::pin(async move {
        let root = host.context.conversation_identity().unwrap().actor;
        let mut pending = VecDeque::new();
        host.input("Run the complete two-request researcher tutorial.").await.unwrap();
        let initial = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        let actors_before = host.context.forest.inspect_host_graph().await.unwrap().into_iter().map(|node| node.actor).collect::<Vec<_>>();
        let preflight = initial.operation("refuse-scalar");
        initial.call("refuse-scalar", &tidepool_testing::fixture_source("bridge/facade/src/actor_host/fixtures/researcher_preflight.hs"));
        let initial = host.while_operation_succeeds(&preflight,
            next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
        let preflight_output = initial.settled_output("refuse-scalar");
        let diagnostic_pages = super::test_campaign::retained_committed_display_texts(
            &host.runtime.store(), &super::test_campaign::actor_output_origin(
                host.runtime.run_identity(), host.context.actor.identity()), &preflight_output);
        assert_eq!(diagnostic_pages.last().map(String::as_str), Some("True"),
            "exact typed scalar refusal required; diagnostics: {diagnostic_pages:?}; {preflight_output}");
        let actors_after = host.context.forest.inspect_host_graph().await.unwrap().into_iter().map(|node| node.actor).collect::<Vec<_>>();
        assert_eq!(actors_after, actors_before, "typed preflight refusal leaves actor inventory unchanged");
        assert!(web.0.lock().unwrap().is_empty(), "preflight invokes no Web handlers");
        let tutorial = initial.operation("researcher-tutorial");
        initial.call("researcher-tutorial", &match script {
            ResearchScript::Tutorial => tidepool_testing::fixture_source("exomonad/examples/workspace/.exomonad/checks/researcher-two-replies.hs"),
            ResearchScript::RefusalControls => tidepool_testing::fixture_source("bridge/facade/src/actor_host/fixtures/researcher_controlled_request.hs"),
        });
        let first = host.while_operation_succeeds(&tutorial,
            select_hosted_script_round(&mut requests, &mut pending, HostedScriptSelection::Any)).await.unwrap();
        if first.host_identity().actor == root {
            panic!("tutorial settled before child inference: {}", first.settled_output("researcher-tutorial"));
        }
        assert_eq!(first.request.model, "gpt-6-luna", "the frozen Luna alias selects its actual provider model");
        let child = first.host_identity();
        let graph = host.context.forest.inspect_host_graph().await.unwrap();
        let selected = graph.iter().filter(|node| host.context.binding(node.actor)
            .and_then(|binding| binding.conversation())
            .is_some_and(|conversation| conversation.identity() == &child)).collect::<Vec<_>>();
        assert_eq!(selected.len(), 1, "one native actor owns the actual child provider request");
        let child_actor = selected[0].actor;
        let child_session = host.context.forest.actor_session(child_actor).await.unwrap().unwrap();
        let root_session = host.context.forest.actor_session(host.context.actor.identity()).await.unwrap().unwrap();
        match child_machines {
            HostedChildMachines::Shared => assert_eq!(child_session, root_session),
            HostedChildMachines::Dedicated => assert_ne!(child_session, root_session),
        }
        let installation = host.context.observer.installation(child_actor).await;
        let retained_a = installation.policy.snapshot_for_request().unwrap();
        let activation_a = retained_a.request_reply_activation().unwrap();
        for declaration in retained_a.tools() {
            if let exomonad_tool::HostedTool::Function(declaration) = declaration {
                let advertised = first.request.tools.iter().find(|tool| tool["name"] == declaration.name).unwrap();
                let schema = harness::finalize::FunctionToolSchema::new(declaration.input_schema.clone()).unwrap();
                assert_eq!(&advertised["parameters"], schema.parameters());
            }
        }
        let installed_requests = tidepool_extract_cmd::extract_spawn_count();
        assert_eq!(reply_properties(&first), ["question", "sources", "summary"]);
        let names = first.request.tools.iter().filter_map(|tool| tool["name"].as_str()).collect::<Vec<_>>();
        assert!(names.contains(&"search") && names.contains(&"open"));
        assert!(!names.contains(&"haskell") && !names.contains(&"haskell_sync"));
        first.function("search-moss", "search", json!({"query":"moss", "limit":3}));
        let opened = host.while_operation_succeeds(&tutorial,
            next_hosted_child_round(&mut requests, &mut pending, &child.actor, &root, "researcher-tutorial")).await.unwrap();
        let search = opened.settled_output("search-moss");
        assert_eq!(search["items"][0]["value"]["Right"][0]["url"], PAGE, "{search}");
        opened.function("open-moss", "open", json!({"url":PAGE}));
        let first_reply = host.while_operation_succeeds(&tutorial,
            next_hosted_child_round(&mut requests, &mut pending, &child.actor, &root, "researcher-tutorial")).await.unwrap();
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), installed_requests,
            "installed search/open handlers submit no extractor requests");
        let open = first_reply.settled_output("open-moss");
        let evidence = &open["items"][0]["value"]["Right"];
        assert_eq!(evidence["content"], BODY, "{open}");
        assert_eq!(evidence["url"], PAGE);
        let reference = evidence["reference"].as_str().unwrap().to_owned();
        let report = json!({"question":"Which plants retain water?", "summary":"Moss absorbs moisture. λ", "sources":[{"sourceUrl":PAGE, "sourceReference":reference}]});
        let second = if script == ResearchScript::RefusalControls {
            let mut first_reply = first_reply;
            for (id, payload) in [
                ("reply-missing", json!({})),
                ("reply-null", json!({"result":null})),
                ("reply-unknown", json!({"result":report,"unexpected":true})),
                ("reply-missing-field", json!({"result":{"question":"q","summary":"s"}})),
                ("reply-decoder-refused", json!({"result":{"question":"q","summary":"","sources":[]}})),
            ] {
                let operation = first_reply.operation(id);
                first_reply.function(id, "reply", payload);
                first_reply = host.while_operation_succeeds(&tutorial,
                    next_hosted_child_round(&mut requests, &mut pending, &child.actor, &root, "researcher-tutorial")).await.unwrap();
                let _failure = recorded_failure(&host, &operation).await;
                assert!(!activation_a.is_settled(), "invalid input cannot claim A");
                assert_eq!(reply_properties(&first_reply), ["question", "sources", "summary"]);
            }
            let before_domain = first_reply.operation("reply-before-domain");
            let domain = first_reply.operation("queued-domain");
            first_reply.functions(&[
                ("reply-before-domain", "reply", json!({"result":report})),
                ("queued-domain", "search", json!({"query":"moss pending control","limit":1})),
            ]);
            first_reply = host.while_operation_succeeds(&tutorial,
                next_hosted_child_round(&mut requests, &mut pending, &child.actor, &root, "researcher-tutorial")).await.unwrap();
            let failure = recorded_failure(&host, &before_domain).await;
            let metadata = failure.metadata().unwrap();
            assert_eq!(metadata["requestReplyRefusal"]["request"], serde_json::to_value(activation_a.request()).unwrap());
            let refusal: exomonad_actor::RequestReplyAdmissionError = serde_json::from_value(
                metadata["requestReplyRefusal"]["refusal"].clone()).unwrap();
            let exomonad_actor::RequestReplyAdmissionError::Pending(owned) = refusal else { panic!("{refusal:?}") };
            assert_eq!(owned.len(), 1);
            assert_eq!(owned[0].invocation, super::embedded_harness::embedded_invocation_context(&child, &domain).unwrap());
            assert_eq!(owned[0].name, "search");
            assert_eq!(owned[0].state, exomonad_actor::HostedInvocationState::Queued);
            assert_eq!(first_reply.settled_output("queued-domain")["status"], "committed");
            assert_eq!(web.0.lock().unwrap().len(), 3, "queued domain handler executes exactly once");
            let accepted = first_reply.operation("report-a");
            let duplicate = first_reply.operation("report-a-distinct");
            first_reply.functions(&[
                ("report-a", "reply", json!({"result":report})),
                ("report-a-distinct", "reply", json!({"result":report})),
            ]);
            let next_root = host.while_operation_succeeds(&tutorial,
                next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
            let failure = recorded_failure(&host, &duplicate).await;
            let refusal: exomonad_actor::RequestReplyAdmissionError = serde_json::from_value(
                failure.metadata().unwrap()["requestReplyRefusal"]["refusal"].clone()).unwrap();
            assert_eq!(refusal, exomonad_actor::RequestReplyAdmissionError::Refused(exomonad_actor::ReplyError::AlreadySettled));
            let accepted = recorded_output(&host, &accepted).await;
            assert_eq!(accepted.terminal, harness::store::TerminalOutcome::Success);
            let value: Value = serde_json::from_str(accepted.item.0["output"].as_str().unwrap()).unwrap();
            assert_eq!(value["status"], serde_json::to_value(tidepool_runtime::session::WorkbenchRunStatus::Replied).unwrap());
            assert_eq!(value["items"].as_array().unwrap().iter().filter(|item| item["terminalTransfer"] ==
                serde_json::to_value(tidepool_runtime::session::WorkbenchTerminalTransfer::ReplyAccepted).unwrap()).count(), 1);
            assert!(activation_a.is_settled());
            let pages = super::test_campaign::retained_committed_display_texts(&host.runtime.store(),
                &super::test_campaign::actor_output_origin(host.runtime.run_identity(), host.context.actor.identity()),
                &next_root.settled_output("researcher-tutorial"));
            assert_eq!(pages.len(), 1);
            assert_eq!(serde_json::from_str::<Value>(&pages[0]).unwrap(), report);
            let next = next_root.operation("researcher-next");
            next_root.call("researcher-next", &tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/fixtures/researcher_controlled_next.hs"));
            let second = host.while_operation_succeeds(&next,
                select_hosted_script_round(&mut requests, &mut pending, HostedScriptSelection::Any)).await.unwrap();
            if second.host_identity().actor == root {
                panic!("B request refused: {}", second.settled_output("researcher-next"));
            }
            let active_b = installation.policy.snapshot_for_request().unwrap().request_reply_activation().unwrap();
            assert_ne!(active_b.request(), activation_a.request());
            assert!(!active_b.is_settled());
            let before_stale = tidepool_extract_cmd::extract_spawn_count();
            let stale = retained_a.dispatch_json_boxed(exomonad_tool::ToolInvocation {
                context: None, name: "reply".into(), arguments: exomonad_tool::ToolArguments::Structured(json!({"result":report})),
            }).await.expect_err("retained A endpoint cannot enter B");
            assert!(matches!(stale, exomonad_actor::ResidentToolError::Invocation(
                exomonad_actor::KernelInvocationFailure::Rejected { actor, ref receipts, .. }
            ) if actor == child_actor && receipts.is_empty()), "{stale}");
            assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before_stale);
            assert!(!active_b.is_settled(), "stale preflight cannot claim B");
            (second, next, "researcher-next")
        } else {
            first_reply.function("report-a", "reply", json!({"result":report}));
            let second = host.while_operation_succeeds(&tutorial,
                next_hosted_child_round(&mut requests, &mut pending, &child.actor, &root, "researcher-tutorial")).await.unwrap();
            (second, tutorial, "researcher-tutorial")
        };
        let (second, terminal_operation, terminal_call) = second;
        assert_eq!(second.host_identity(), child, "same reusable actor/incarnation");
        assert_eq!(reply_properties(&second), ["answer", "citations"]);
        let follow_up = json!({"answer":"Moss leaves absorb moisture.", "citations":[PAGE]});
        second.function("report-b", "reply", json!({"result":follow_up}));
        let completed = host.while_operation_succeeds(&terminal_operation,
            next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
        completed.assert_committed(terminal_call);
        let output = completed.settled_output(terminal_call);
        let pages = super::test_campaign::retained_committed_display_texts(
            &host.runtime.store(), &harness::store::actor_output::ActorOutputOrigin {
                run: host.runtime.run_identity().to_owned(), native_actor: host.context.actor.identity().id.0,
                incarnation: host.context.actor.identity().incarnation.0,
            }, &output);
        assert_eq!(pages.len(), 2);
        assert_eq!(serde_json::from_str::<Value>(&pages[0]).unwrap(), if script == ResearchScript::Tutorial { report.clone() } else { json!(true) });
        assert_eq!(serde_json::from_str::<Value>(&pages[1]).unwrap(), follow_up);
        assert!(pending.is_empty(), "no extra inference after either terminal reply");
        assert!(requests.try_recv().is_err(), "no successor child request after settlement");
        let requests = web.0.lock().unwrap();
        assert_eq!(requests.len(), if script == ResearchScript::Tutorial { 2 } else { 3 });
        assert!(matches!(requests[0].0, WebOperation::Search));
        assert_eq!(requests[0].1["query"], "moss");
        assert_eq!(requests[0].1["max_results"], 3);
        assert!(matches!(requests[1].0, WebOperation::Extract));
        assert_eq!(requests[1].1["urls"], json!([PAGE]));
        drop(requests);
        completed.finish();
    })).await;
}

#[tokio::test]
async fn source_backed_web_researcher_settles_two_typed_requests() {
    research_history(HostedChildMachines::Shared, ResearchScript::Tutorial).await;
}

#[tokio::test]
async fn dedicated_web_researcher_uses_the_same_injected_transport() {
    research_history(HostedChildMachines::Dedicated, ResearchScript::Tutorial).await;
}

#[tokio::test]
async fn source_backed_reply_refuses_invalid_and_queued_domain_then_settles_once() {
    research_history(HostedChildMachines::Shared, ResearchScript::RefusalControls).await;
}

async fn recorded_output(
    host: &HostedTestRuntime,
    operation: &harness::model::OperationId,
) -> harness::store::RecordedToolOutput {
    host.while_host_running(tokio::time::timeout(
        super::test_campaign::COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
        async {
            loop {
                if let Some(recorded) = host
                    .runtime
                    .store()
                    .replay_tool_output_operation(operation)
                    .unwrap()
                {
                    return recorded;
                }
                tokio::task::yield_now().await;
            }
        },
    ))
    .await
    .unwrap()
    .expect("exact original operation must reach durable settlement")
}

async fn recorded_failure(
    host: &HostedTestRuntime,
    operation: &harness::model::OperationId,
) -> harness::provider::ToolFailure {
    let recorded = recorded_output(host, operation).await;
    let harness::store::TerminalOutcome::Failure(failure) = recorded.terminal else {
        panic!(
            "expected exact failed original operation {operation:?}: {:?}",
            recorded.terminal
        )
    };
    failure
}
