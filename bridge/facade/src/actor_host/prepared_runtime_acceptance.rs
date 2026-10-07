//! Completed source entries through real child admission and native tools.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    commit_workspace, hosted_script_provider, hosted_test_settings, next_hosted_script_round,
};
use super::*;
use std::collections::{HashSet, VecDeque};
use std::time::Instant;
use tracing_subscriber::prelude::*;

#[derive(Default)]
struct Fields(BTreeMap<String, String>);

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
}

#[derive(Default)]
struct Observations {
    requests: Vec<BTreeMap<String, String>>,
    starts: HashMap<String, (Instant, usize)>,
    installs: HashMap<String, (BTreeMap<String, String>, usize)>,
}

#[derive(Clone, Default)]
struct SetupTrace(Arc<Mutex<Observations>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SetupTrace {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut state = self.0.lock();
        if fields.0.contains_key("compile_request")
            && fields.0.contains_key("request_ordinal")
            && event.metadata().target() == "tidepool_extract_cmd::endpoint"
        {
            assert_eq!(
                fields.0["transport"], "daemon",
                "the actual matched daemon submits this request"
            );
            state.requests.push(fields.0.clone());
        }
        match fields.0.get("phase").map(String::as_str) {
            Some("child_launch_requested") => {
                let label = fields.0["label"].clone();
                let count = state.requests.len();
                assert!(
                    state
                        .starts
                        .insert(label, (Instant::now(), count))
                        .is_none(),
                    "the measured launch labels must be unique"
                );
            }
            Some("toolset_installed") => {
                let actor = fields.0["actor"].clone();
                let count = state.requests.len();
                assert!(
                    state.installs.insert(actor, (fields.0, count)).is_none(),
                    "one initial installation for each exact actor"
                );
            }
            _ => {}
        }
    }
}

#[derive(serde::Serialize)]
struct Progress {
    schema: u8,
    expected_children: usize,
    observed_children: usize,
    observed_installations: usize,
    deployment_originals_verified: usize,
    observed_native_replies: usize,
    distinct_installation_scopes: usize,
    compiler_requests_during_setup: usize,
    quotation_executions_before: Option<usize>,
    quotation_executions_observed: Option<usize>,
    parent_result_verified: bool,
    quotation_execution_unchanged: bool,
    completed: bool,
}

impl Progress {
    fn new(expected_children: usize) -> Self {
        Self {
            schema: 1,
            expected_children,
            observed_children: 0,
            observed_installations: 0,
            deployment_originals_verified: 0,
            observed_native_replies: 0,
            distinct_installation_scopes: 0,
            compiler_requests_during_setup: 0,
            quotation_executions_before: None,
            quotation_executions_observed: None,
            parent_result_verified: false,
            quotation_execution_unchanged: false,
            completed: false,
        }
    }

    fn report(&self) {
        eprintln!(
            "prepared-runtime-progress {}",
            serde_json::to_string(self).unwrap()
        );
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.report();
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires exclusive matched compiler daemon and prepared root entry"]
async fn production_prepared_toolset_one_child_executes_original_native_probe() {
    prepared_children_execute_original_native_probe(1).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires exclusive matched compiler daemon and prepared root entry"]
async fn production_prepared_toolset_twenty_children_execute_original_native_probe() {
    prepared_children_execute_original_native_probe(20).await;
}

async fn prepared_children_execute_original_native_probe(expected_children: usize) {
    let mut progress = Progress::new(expected_children);
    progress.report();
    assert!(
        std::env::var_os("TIDEPOOL_PREPARED_ROOT_ENTRY").is_some(),
        "the campaign must select its matched bundle-owned fixed driver"
    );
    let socket = std::path::PathBuf::from(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).expect("owned matched daemon"),
    );
    let endpoint = tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap();
    let observations = SetupTrace::default();
    tracing_subscriber::registry()
        .with(observations.clone())
        .with(tracing_subscriber::fmt::layer().json().with_ansi(false))
        .with(tracing_subscriber::EnvFilter::new("warn,tidepool_extract_cmd::endpoint=info,exomonad_actor::workbench_phase=info,tidepool::actor_host::startup=info,tidepool_runtime::prepared_install=info,tidepool_codegen::prepared_compile=info"))
        .try_init().expect("one isolated measurement process owns its subscriber");
    let mut files = match std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
        Some(root) => tempfile::Builder::new()
            .prefix("prepared-quotation-")
            .tempdir_in(root)
            .unwrap(),
        None => tempfile::tempdir().unwrap(),
    };
    files.disable_cleanup(std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").is_some());
    let input = files.path().join("external-input");
    std::fs::write(&input, "41").unwrap();
    let settings = hosted_test_settings(&files, 2);
    let authored_settings = settings.clone();
    let quotation_input = input.clone();
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_prepared_configured(&settings, &provider, move |config| {
        let authored = config.workspace.join(".exomonad");
        std::fs::create_dir_all(&authored).unwrap();
        std::fs::write(
            authored.join("QuotedProvider.hs"),
            include_str!("../../../../exomonad/actor/src/fixtures/quoted-agent-provider.hs"),
        )
        .unwrap();
        std::fs::write(
            authored.join("PreparedRuntimeSpec.hs"),
            include_str!("prepared_runtime_spec.hs")
                .replace("{quotation-input}", quotation_input.to_str().unwrap()),
        )
        .unwrap();
        crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
            project.launch.embedded = Some(authored_settings);
            project.haskell.source_roots = vec![".".into()];
            project.haskell.modules = vec!["PreparedRuntimeSpec".into()];
            project.haskell.spec = Some("PreparedRuntimeSpec.agentSpec".into());
            project.preparation.roles = vec![exomonad_actor::ActorRole::Research];
        });
        commit_workspace(&config.workspace);
    })
    .await
    .expect("actual production preparation and selected deployment start the host");
    host.run_scenario(|host| Box::pin(async move {
        let scenario = async move {
        let frozen = host.context.config.workspace_inputs.as_ref()
            .expect("the actual host selected its completed workspace");
        let completed_entries = frozen.completed_entry_selections().expect("completed installer inventory");
        assert!(!completed_entries.is_empty());
        let research_coverage = frozen.prepared_toolset_coverage()
            .expect("completed toolset coverage")
            .iter().find(|entry| entry.profile == crate::exomonad::PreparationProfile::Public(
                exomonad_tool::PublicActorProfile::Research))
            .expect("the configured public research profile was prepared");
        assert_eq!(research_coverage.requested_effects,
            exomonad_tool::PublicActorProfile::Research.effect_keys());
        let prepared_executions = std::fs::read_to_string(input.with_extension("executions")).unwrap();
        assert!(!prepared_executions.is_empty(), "the original producer executed the real quoter");
        progress.quotation_executions_before = Some(prepared_executions.lines().count());
        std::fs::write(&input, "42").unwrap();
        host.input("Execute prepared child native probes.").await.unwrap();
        let root = harness::model::AgentPath("/root".into());
        let mut pending = VecDeque::new();
        let children_source = include_str!("prepared_runtime_children.hs")
            .replace("{prepared-child-count}", &expected_children.to_string());
        next_hosted_script_round(&mut requests, &mut pending, &root).await
            .call("prepared-children", &children_source);
        let mut children = HashSet::new();
        let mut scopes = HashSet::new();
        let mut shared = None;
        let mut rows = Vec::new();
        for ordinal in 1..=expected_children {
            let label = format!("prepared-runtime/probe-{ordinal}/prepared-child-{ordinal}");
            let round = match pending.pop_front() {
                Some(round) => round,
                None => tokio::time::timeout(std::time::Duration::from_secs(300), requests.recv())
                    .await.unwrap().expect("the actual admitted child requests its provider"),
            };
            let origin = round.origin();
            if origin.actor() == &root {
                round.assert_committed("prepared-children");
            }
            assert_ne!(origin.actor(), &root, "parent waits for this actual child reply");
            let nodes = host.context.forest.inspect_host_graph();
            let matching = nodes.iter().filter(|node| node.label == label).collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "unique exact host graph child {label}");
            let child = matching[0];
            assert_eq!(child.creator, Some(host.context.actor.identity()));
            assert!(children.insert(child.actor));
            progress.observed_children = children.len();
            progress.report();
            let binding = host.context.binding(child.actor).expect("production child attachment");
            let conversation = binding.conversation().unwrap();
            assert_eq!(conversation.identity().actor, *origin.actor());
            let installed = host.context.observer.installation(child.actor).await;
            progress.observed_installations += 1;
            let (elapsed, compiler_requests, details) = {
                let state = observations.0.lock();
                let (start, before) = state.starts.get(&label).expect("real pre-lookup launch request");
                let (details, after) = state.installs.get(&child.actor.to_string())
                    .expect("same exact actor executed its installer");
                (installed.installed_at.duration_since(*start).as_nanos(), state.requests[*before..*after].to_vec(), details.clone())
            };
            assert!(scopes.insert(details["installation_scope"].clone()), "fresh installation heap scope");
            progress.distinct_installation_scopes = scopes.len();
            progress.compiler_requests_during_setup += compiler_requests.len();
            progress.report();
            let current_executions = std::fs::read_to_string(input.with_extension("executions")).unwrap();
            progress.quotation_executions_observed = Some(current_executions.lines().count());
            progress.quotation_execution_unchanged = current_executions == prepared_executions;
            progress.report();
            assert!(compiler_requests.is_empty(), "completed child admission must not compile installer source: {compiler_requests:?}");
            assert!(progress.quotation_execution_unchanged,
                "child admission must not execute the external quoter again: {current_executions}");
            assert!(!installed.checkpoint);
            assert_eq!(installed.context_parent, None, "selected provider context");
            assert_eq!(installed.role, exomonad_actor::ActorRole::Research);
            let acquisition = installed.acquisition.as_ref().expect("installed source acquisition");
            let exomonad_actor::ToolsetAcquisition::DeploymentOriginal { recipe, original } = acquisition else {
                panic!("prepared child must load its deployment original: {acquisition:?}");
            };
            assert_eq!(completed_entries.get(recipe), Some(original),
                "the actual installed original belongs to the frozen completed inventory");
            assert_eq!((recipe, original), (&research_coverage.recipe, &research_coverage.original),
                "the child selects the configured research specialization");
            progress.deployment_originals_verified += 1;
            progress.report();
            assert!(installed.tools.iter().any(|tool| matches!(tool,
                exomonad_tool::HostedTool::Function(tool) if tool.name == "probe" && tool.description == "41")));
            let identity = ["prepared_owner", "image_registry", "source_revision"]
                .map(|field| details[field].clone());
            if let Some(previous) = &shared { assert_eq!(&identity, previous); }
            shared = Some(identity);
            round.function("prepared-native-probe", "probe", serde_json::json!({"topic": label}));
            let replied = next_hosted_script_round(&mut requests, &mut pending, origin.actor()).await;
            let output = replied.settled_output("prepared-native-probe");
            assert_eq!(output["status"], "replied", "{output}");
            assert_eq!(output["items"].as_array().unwrap().last().unwrap()["terminalTransfer"], "replyAccepted", "{output}");
            replied.finish();
            progress.observed_native_replies += 1;
            progress.report();
            let row = serde_json::json!({"schema":1,"composition":"engine-store-prepared-child",
                "ordinal":ordinal,"actor":child.actor,"provider_origin":origin,"setup_elapsed_ns":elapsed,
                "setup_end":"actual_policy_installed","setup_start":"child_launch_requested",
                "compiler_requests_during_setup":compiler_requests,"installation":details,
                "acquisition":acquisition,"completed_inventory_match":true,
                "native_reply_accepted":true,"producer":endpoint.producer_hex()});
            eprintln!("prepared-runtime-child {row}");
            rows.push(row);
        }
        let completed = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        completed.assert_value("prepared-children", &format!("[{}]", vec!["41"; children.len()].join(",")));
        progress.parent_result_verified = true;
        completed.finish();
        assert_eq!(children.len(), expected_children);
        assert_eq!(rows.len(), expected_children);
        assert_eq!(progress.deployment_originals_verified, expected_children);
        assert_eq!(scopes.len(), children.len());
        assert_eq!(std::fs::read_to_string(input.with_extension("executions")).unwrap(), prepared_executions,
            "child installers select the completed original without replaying external input42");
        progress.quotation_execution_unchanged = true;
        let mut durations = rows.iter().map(|row| row["setup_elapsed_ns"].as_u64().unwrap()).collect::<Vec<_>>();
        durations.sort_unstable();
        let p95 = durations[(durations.len() * 95).div_ceil(100) - 1];
        eprintln!("prepared-runtime-summary {}", serde_json::json!({"observed_children":children.len(),
            "observed_native_replies":rows.len(),"distinct_installation_scopes":scopes.len(),
            "setup_p95_ns":p95,"target_ns":1_000_000_000u64,"target_met":p95<1_000_000_000,
            "first_preparation_reported_separately":true}));
        progress.completed = true;
        progress.report();
        };
        host.while_host_running(scenario).await
            .expect("the production host remains available through every native reply");
    })).await;
}
