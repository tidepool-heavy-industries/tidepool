//! Execute the exact published tutorial through real source capture, Harness,
//! installed Sync handlers, Web normalization and typed request settlement.
use super::hosted_test_context::{HostedChildMachines, HostedTestRuntime};
use super::test_campaign::{
    commit_workspace, hosted_script_provider, hosted_test_settings, next_hosted_script_round,
    select_hosted_script_round, HostedScriptRound, HostedScriptSelection,
};
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

fn configure_research(config: &mut super::ActorHostConfig) {
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
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec!["Project.Researcher".into(), "AgentSpec".into()];
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

async fn research_history(child_machines: HostedChildMachines) {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 4);
    let (provider, mut requests) = hosted_script_provider();
    let web = Arc::new(ResearchWeb::default());
    let host = HostedTestRuntime::start_configured_with_web(
        &settings,
        &provider,
        web.clone(),
        child_machines,
        configure_research,
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
        initial.call("researcher-tutorial", &tidepool_testing::fixture_source("exomonad/examples/workspace/.exomonad/checks/researcher-two-replies.hs"));
        let first = host.while_operation_succeeds(&tutorial,
            select_hosted_script_round(&mut requests, &mut pending, HostedScriptSelection::OtherThan(&root))).await.unwrap();
        let child = first.host_identity();
        let installed_requests = tidepool_extract_cmd::extract_spawn_count();
        assert_eq!(reply_properties(&first), ["question", "sources", "summary"]);
        let names = first.request.tools.iter().filter_map(|tool| tool["name"].as_str()).collect::<Vec<_>>();
        assert!(names.contains(&"search") && names.contains(&"open"));
        assert!(!names.contains(&"haskell") && !names.contains(&"haskell_sync"));
        first.function("search-moss", "search", json!({"query":"moss", "limit":3}));
        let opened = host.while_operation_succeeds(&tutorial,
            next_hosted_script_round(&mut requests, &mut pending, &child.actor)).await.unwrap();
        let search = opened.settled_output("search-moss");
        assert_eq!(search["items"][0]["value"]["Right"][0]["url"], PAGE, "{search}");
        opened.function("open-moss", "open", json!({"url":PAGE}));
        let first_reply = host.while_operation_succeeds(&tutorial,
            next_hosted_script_round(&mut requests, &mut pending, &child.actor)).await.unwrap();
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), installed_requests,
            "installed search/open handlers submit no extractor requests");
        let open = first_reply.settled_output("open-moss");
        let evidence = &open["items"][0]["value"]["Right"];
        assert_eq!(evidence["content"], BODY, "{open}");
        assert_eq!(evidence["url"], PAGE);
        let reference = evidence["reference"].as_str().unwrap().to_owned();
        let report = json!({"question":"Which plants retain water?", "summary":"Moss absorbs moisture. λ", "sources":[{"sourceUrl":PAGE, "sourceReference":reference}]});
        first_reply.function("report-a", "reply", json!({"result":report}));
        let second = host.while_operation_succeeds(&tutorial,
            next_hosted_script_round(&mut requests, &mut pending, &child.actor)).await.unwrap();
        assert_eq!(second.host_identity(), child, "same reusable actor/incarnation");
        assert_eq!(reply_properties(&second), ["answer", "citations"]);
        let follow_up = json!({"answer":"Moss leaves absorb moisture.", "citations":[PAGE]});
        second.function("report-b", "reply", json!({"result":follow_up}));
        let completed = host.while_operation_succeeds(&tutorial,
            next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
        completed.assert_committed("researcher-tutorial");
        let output = completed.settled_output("researcher-tutorial");
        let pages = super::test_campaign::retained_committed_display_texts(
            &host.runtime.store(), &harness::store::actor_output::ActorOutputOrigin {
                run: host.runtime.run_identity().to_owned(), native_actor: host.context.actor.identity().id.0,
                incarnation: host.context.actor.identity().incarnation.0,
            }, &output);
        assert_eq!(pages.len(), 2);
        assert_eq!(serde_json::from_str::<Value>(&pages[0]).unwrap(), report);
        assert_eq!(serde_json::from_str::<Value>(&pages[1]).unwrap(), follow_up);
        assert!(pending.is_empty(), "no extra inference after either terminal reply");
        assert!(requests.try_recv().is_err(), "no successor child request after settlement");
        let requests = web.0.lock().unwrap();
        assert_eq!(requests.len(), 2);
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
    research_history(HostedChildMachines::Shared).await;
}

#[tokio::test]
async fn dedicated_web_researcher_uses_the_same_injected_transport() {
    research_history(HostedChildMachines::Dedicated).await;
}
