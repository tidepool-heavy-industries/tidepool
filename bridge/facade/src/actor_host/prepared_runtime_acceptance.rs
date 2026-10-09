//! Prepared root originals and supplied compiled child specs through native replies.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    commit_workspace, hosted_script_provider, hosted_test_settings, next_hosted_script_round,
};
use super::*;
use std::collections::{HashSet, VecDeque};
use std::time::Instant;
use tracing_subscriber::prelude::*;

#[derive(Clone, Default)]
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
    admissions: Vec<BTreeMap<String, String>>,
    starts: HashMap<String, ChildLaunch>,
    first_child_launch_executions: Option<String>,
    installs: HashMap<String, (BTreeMap<String, String>, usize)>,
    installer_phases: HashMap<String, (BTreeMap<String, String>, Vec<BTreeMap<String, String>>)>,
}

struct ChildLaunch {
    at: Instant,
    compiler_requests: usize,
    quotation_executions: String,
}

#[derive(Clone)]
struct SetupTrace {
    observations: Arc<Mutex<Observations>>,
    quotation_counter: PathBuf,
}

impl SetupTrace {
    fn new(quotation_counter: PathBuf) -> Self {
        Self {
            observations: Arc::default(),
            quotation_counter,
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for SetupTrace
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let span = context.span(id).expect("observed registered span");
        if attributes.metadata().name() == "compiled_spec_install" {
            let actor = fields.0["actor"].clone();
            span.extensions_mut().insert(fields.clone());
            assert!(
                self.observations
                    .lock()
                    .installer_phases
                    .insert(actor, (fields.0, Vec::new()))
                    .is_none(),
                "one initial compiled installer phase per exact child"
            );
        } else if attributes.metadata().name() == "compile_request" {
            for ancestor in span.scope() {
                if ancestor.name() == "compiled_spec_install" {
                    let actor = ancestor
                        .extensions()
                        .get::<Fields>()
                        .expect("installer correlation fields")
                        .0["actor"]
                        .clone();
                    self.observations
                        .lock()
                        .installer_phases
                        .get_mut(&actor)
                        .expect("the installer phase began before its compiler request")
                        .1
                        .push(fields.0);
                    break;
                }
            }
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut state = self.observations.lock();
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
            Some("compiler_transaction_admission") => {
                state.admissions.push(fields.0);
            }
            Some("child_launch_requested") => {
                let label = fields.0["label"].clone();
                let at = Instant::now();
                let quotation_executions = std::fs::read_to_string(&self.quotation_counter)
                    .expect("read the external quoter counter at the actual child launch");
                state
                    .first_child_launch_executions
                    .get_or_insert_with(|| quotation_executions.clone());
                let launch = ChildLaunch {
                    at,
                    compiler_requests: state.requests.len(),
                    quotation_executions,
                };
                assert!(
                    state.starts.insert(label, launch).is_none(),
                    "the measured launch labels must be unique"
                );
            }
            Some("toolset_installed" | "explicit_toolset_installed") => {
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
    root_deployment_original_verified: bool,
    explicit_installers_verified: usize,
    compiler_requests_during_installers: usize,
    observed_native_replies: usize,
    distinct_installation_scopes: usize,
    compiler_requests_during_setup: usize,
    preparation_requests_observed: usize,
    quotation_executions_before: Option<usize>,
    quotation_executions_at_first_child_launch: Option<usize>,
    parent_quotation_executions: Option<usize>,
    quotation_executions_during_children: Option<usize>,
    quotation_executions_observed: Option<usize>,
    parent_result_verified: bool,
    quotation_execution_unchanged: bool,
    completed: bool,
}

impl Progress {
    fn new(expected_children: usize) -> Self {
        Self {
            schema: 2,
            expected_children,
            observed_children: 0,
            observed_installations: 0,
            root_deployment_original_verified: false,
            explicit_installers_verified: 0,
            compiler_requests_during_installers: 0,
            observed_native_replies: 0,
            distinct_installation_scopes: 0,
            compiler_requests_during_setup: 0,
            preparation_requests_observed: 0,
            quotation_executions_before: None,
            quotation_executions_at_first_child_launch: None,
            parent_quotation_executions: None,
            quotation_executions_during_children: None,
            quotation_executions_observed: None,
            parent_result_verified: false,
            quotation_execution_unchanged: false,
            completed: false,
        }
    }

    fn observe_quotations(&mut self, prepared: &str, first_launch: &str, current: &str) {
        let preparation_count = prepared.lines().count();
        let first_launch_count = first_launch.lines().count();
        let current_count = current.lines().count();
        self.quotation_executions_at_first_child_launch = Some(first_launch_count);
        self.parent_quotation_executions = Some(
            first_launch_count
                .checked_sub(preparation_count)
                .expect("the quoter counter cannot shrink before the first child launch"),
        );
        self.quotation_executions_during_children = Some(
            current_count
                .checked_sub(first_launch_count)
                .expect("the quoter counter cannot shrink during child execution"),
        );
        self.quotation_executions_observed = Some(current_count);
        self.quotation_execution_unchanged = current == prepared;
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
#[ignore = "requires matched prepared bundle and owned resident compiler"]
async fn production_builtin_toolset_immediate_and_durable_launches_are_fresh() {
    use crate::exomonad::workspace::{prepare_workspace, FrozenWorkspace};
    use harness::model::AgentPath;
    use std::os::unix::fs::PermissionsExt;
    struct WritableOnDrop(PathBuf);
    impl Drop for WritableOnDrop {
        fn drop(&mut self) {
            fn restore(path: &Path) {
                let Ok(metadata) = std::fs::symlink_metadata(path) else {
                    return;
                };
                if metadata.file_type().is_symlink() {
                    return;
                }
                let mut permissions = metadata.permissions();
                permissions
                    .set_mode(permissions.mode() | if metadata.is_dir() { 0o700 } else { 0o600 });
                let _ = std::fs::set_permissions(path, permissions);
                if metadata.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(path) {
                        for entry in entries.flatten() {
                            restore(&entry.path());
                        }
                    }
                }
            }
            restore(&self.0);
        }
    }
    assert!(std::env::var_os("TIDEPOOL_PREPARED_ROOT_ENTRY").is_some());
    assert!(std::env::var_os("TIDEPOOL_PREPARED_BUILTIN_ENTRIES").is_some());
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    crate::exomonad::write_fixture_project_config(
        &repository.path().join(".exomonad"),
        "test-model",
        |project| {
            project.launch.embedded = Some(settings.clone());
        },
    );
    commit_workspace(repository.path());
    let directory = Arc::new(
        tidepool_atomic_write::DirectoryAnchor::open_existing(files.path())
            .unwrap()
            .child("prepared-defaults")
            .unwrap(),
    );
    let _release = WritableOnDrop(directory.path().to_owned());
    let observations = SetupTrace::new(files.path().join("unused-quotation-counter"));
    tracing_subscriber::registry().with(observations.clone())
        .with(tracing_subscriber::EnvFilter::new("warn,tidepool_extract_cmd::endpoint=info,exomonad_actor::workbench_phase=info,tidepool::actor_host::startup=info"))
        .try_init().expect("isolated cohort owns its observer");
    let prepared = prepare_workspace(repository.path(), Arc::clone(&directory))
        .await
        .unwrap();
    assert!(
        observations.observations.lock().requests.is_empty(),
        "packaged default preparation submits no compiler request"
    );
    let mut actors = HashSet::new();
    let mut run_journals = HashSet::new();
    for immediate in [true, false] {
        let (provider, mut requests) = hosted_script_provider();
        let before_start = observations.observations.lock().requests.len();
        let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
            config.workspace = repository.path().to_owned();
            let inputs = if immediate {
                prepared
                    .select_for_run(repository.path(), config.run_directory.path())
                    .unwrap()
            } else {
                FrozenWorkspace::select_prepared(
                    repository.path(),
                    config.run_directory.path(),
                    Some(directory.path()),
                )
                .unwrap()
            };
            assert_eq!(inputs.prepared_toolset.is_some(), immediate);
            config.haskell_root = inputs.runtime_actors();
            config.workspace_inputs = Some(inputs);
        })
        .await
        .unwrap();
        let run_id = host
            .context
            .config
            .run_directory
            .path()
            .file_name()
            .unwrap();
        let journal =
            crate::exomonad::exomonad_journal_path(repository.path(), run_id.to_str().unwrap());
        assert!(journal.is_file(), "each fresh host owns its run journal");
        assert!(
            run_journals.insert(journal),
            "fresh hosts sharing a workspace must have distinct run journals"
        );
        assert!(
            actors.insert(host.context.actor.identity()),
            "each host creates a fresh actor identity"
        );
        assert_eq!(
            observations.observations.lock().requests.len(),
            before_start,
            "both immediate and durable startup use packaged code"
        );
        host.run_scenario(|host| Box::pin(async move {
            let frozen = host.context.config.workspace_inputs.as_ref().unwrap();
            assert!(frozen.completed_entry_selections().unwrap().is_empty(), "built-ins carry no workspace-original UUID");
            let installation = host.context.observer.installation(host.context.actor.identity()).await;
            assert_eq!(installation.acquisition.as_ref().and_then(exomonad_actor::ToolsetAcquisition::selection),
                Some(frozen.prepared_toolset_coverage().unwrap()[0].program.clone()));
            assert!(matches!(installation.acquisition,
                Some(exomonad_actor::ToolsetAcquisition::BuiltinDeploymentProgram { .. })));
            super::scripted_recursive_acceptance::admit_http_input(host).await;
            let root = AgentPath("/root".into());
            let mut pending = VecDeque::new();
            if !immediate {
                next_hosted_script_round(&mut requests, &mut pending, &root).await
                    .call("fresh-state-control", "display onlyFirstHost");
                let refused = next_hosted_script_round(&mut requests, &mut pending, &root).await;
                refused.assert_failure("fresh-state-control", "not in scope");
                refused.call("first-notebook", "freshValue <- pure (41 :: Int)\ndisplay freshValue");
            } else {
                next_hosted_script_round(&mut requests, &mut pending, &root).await
                    .call("first-notebook", "onlyFirstHost <- pure (41 :: Int)\nfreshValue <- pure (41 :: Int)\ndisplay freshValue");
            }
            let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            first.assert_value("first-notebook", "41");
            first.call("warm-notebook", "display freshValue");
            let warm = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            warm.assert_value("warm-notebook", "41");
            warm.finish();
        })).await;
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
    let observations = SetupTrace::new(input.with_extension("executions"));
    tracing_subscriber::registry()
        .with(observations.clone())
        .with(tracing_subscriber::fmt::layer().json().with_ansi(false))
        .with(tracing_subscriber::EnvFilter::new("warn,tidepool_extract_cmd::endpoint=info,exomonad_actor::workbench_phase=info,tidepool::actor_host::startup=info,tidepool_runtime::prepared_install=info,tidepool_codegen::prepared_compile=info"))
        .try_init().expect("one isolated measurement process owns its subscriber");
    let settings = hosted_test_settings(&files, 2);
    let authored_settings = settings.clone();
    let quotation_input = input.clone();
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_prepared_configured(&settings, &provider, move |config| {
        let authored = config.workspace.join(".exomonad");
        std::fs::create_dir_all(&authored).unwrap();
        std::fs::write(
            authored.join("QuotedProvider.hs"),
            tidepool_testing::fixture_source(
                "exomonad/actor/src/fixtures/quoted-agent-provider.hs",
            ),
        )
        .unwrap();
        std::fs::write(
            authored.join("PreparedRuntimeSpec.hs"),
            tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/prepared_runtime_spec.hs",
            )
            .replace("{quotation-input}", quotation_input.to_str().unwrap()),
        )
        .unwrap();
        crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
            project.launch.embedded = Some(authored_settings);
            project.haskell.source_roots = vec![".".into()];
            project.haskell.modules = vec!["PreparedRuntimeSpec".into()];
            project.haskell.spec = Some("PreparedRuntimeSpec.agentSpec".into());
        });
        commit_workspace(&config.workspace);
    })
    .await
    .expect("actual production preparation and selected deployment start the host");
    host.run_scenario(|host| Box::pin(async move {
        let scenario = async move {
        {
            let state = observations.observations.lock();
            assert!(!state.requests.is_empty(),
                "the observer must see actual production preparation before proving no child setup requests");
            assert!(!state.admissions.is_empty(), "the actual compiler transaction was observed");
            for admission in &state.admissions {
                assert_eq!(admission["transport"], "daemon");
                assert_eq!(admission["producer"], endpoint.producer_hex());
                assert_eq!(admission["endpoint"], endpoint.to_hex());
            }
            let mut correlations = HashSet::new();
            for request in &state.requests {
                assert!(!request["compile_request"].is_empty(), "actual request correlation");
                let epoch = &request["daemon_epoch"];
                assert_eq!(epoch.len(), 64, "actual daemon epoch identity");
                assert!(epoch.bytes().all(|byte| byte.is_ascii_hexdigit()));
                let admission = request["admission_id"].parse::<u64>().unwrap();
                let ordinal = request["request_ordinal"].parse::<u64>().unwrap();
                assert!(admission > 0 && ordinal > 0, "actual admitted request identity");
                assert!(correlations.insert((epoch, admission, ordinal)),
                    "each observed request has a unique admitted daemon correlation");
            }
            progress.preparation_requests_observed = state.requests.len();
        }
        progress.report();
        let frozen = host.context.config.workspace_inputs.as_ref()
            .expect("the actual host selected its completed workspace");
        let completed_entries = frozen.completed_entry_selections().expect("completed installer inventory");
        assert!(!completed_entries.is_empty());
        let coverage = frozen.prepared_toolset_coverage()
            .expect("completed root toolset coverage");
        assert_eq!(coverage.len(), 1, "one actual root toolset is prepared");
        let root_coverage = &coverage[0];
        assert_eq!(
            root_coverage.requested_effects,
            exomonad_actor::ActorCapabilities::default().effect_keys()
        );
        let root_installation = host.context.observer.installation(host.context.actor.identity()).await;
        let root_acquisition = root_installation.acquisition.as_ref()
            .expect("the source-prepared root retains its actual acquisition");
        let exomonad_actor::ToolsetProgramSelection::WorkspaceOriginal { recipe, original } =
            root_acquisition.selection().expect("prepared program selection") else {
            panic!("the authored quoted spec must use its workspace original");
        };
        assert_eq!(completed_entries.get(&recipe), Some(&original),
            "the installed root original belongs to the completed inventory");
        assert_eq!(root_acquisition.selection(), Some(root_coverage.program.clone()));
        progress.root_deployment_original_verified = true;
        let prepared_executions = std::fs::read_to_string(input.with_extension("executions")).unwrap();
        assert!(!prepared_executions.is_empty(), "the original producer executed the real quoter");
        progress.quotation_executions_before = Some(prepared_executions.lines().count());
        std::fs::write(&input, "42").unwrap();
        host.input("Execute prepared child native probes.").await.unwrap();
        let root = harness::model::AgentPath("/root".into());
        let mut pending = VecDeque::new();
        let children_source = tidepool_testing::fixture_source("bridge/facade/src/actor_host/prepared_runtime_children.hs")
            .replace("{prepared-child-count}", &expected_children.to_string());
        next_hosted_script_round(&mut requests, &mut pending, &root).await
            .call("prepared-children", &children_source);
        let mut children = HashSet::new();
        let mut scopes = HashSet::new();
        let mut rows = Vec::new();
        for ordinal in 1..=expected_children {
            let label = format!("prepared-child-{ordinal}");
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
            let (elapsed, compiler_requests, installer_details, installer_compiler_requests, details, launch_executions, first_launch_executions) = {
                let state = observations.observations.lock();
                let start = state.starts.get(&label).expect("real pre-lookup launch request");
                let (details, after) = state.installs.get(&child.actor.to_string())
                    .expect("same exact actor executed its installer");
                let (installer_details, installer_requests) = state.installer_phases.get(&child.actor.to_string())
                    .expect("same exact child executed its supplied compiled installer");
                (installed.installed_at.duration_since(start.at).as_nanos(),
                    state.requests[start.compiler_requests..*after].to_vec(),
                    installer_details.clone(), installer_requests.clone(), details.clone(),
                    start.quotation_executions.clone(),
                    state.first_child_launch_executions.clone().expect("the first actual child launch was observed"))
            };
            assert!(scopes.insert(details["installation_scope"].clone()), "fresh installation heap scope");
            progress.distinct_installation_scopes = scopes.len();
            progress.compiler_requests_during_setup += compiler_requests.len();
            progress.compiler_requests_during_installers += installer_compiler_requests.len();
            progress.report();
            let current_executions = std::fs::read_to_string(input.with_extension("executions")).unwrap();
            progress.observe_quotations(&prepared_executions, &first_launch_executions, &current_executions);
            progress.report();
            assert!(compiler_requests.is_empty(), "completed child admission must not compile installer source: {compiler_requests:?}");
            assert!(installer_compiler_requests.is_empty(),
                "supplied compiled installer must not submit compiler work: {installer_compiler_requests:?}");
            assert_eq!(installer_details["origin"], "explicit_live");
            assert_eq!(installer_details["actor"], child.actor.to_string());
            assert_eq!(installer_details["installation"], details["install"]);
            assert_eq!(details["phase"], "explicit_toolset_installed");
            assert_eq!(first_launch_executions, prepared_executions,
                "parent public spec import must not replay its original quotation before the first child launch");
            assert_eq!(launch_executions, first_launch_executions,
                "no child may replay the quoter between the first launch and launch of {label}");
            assert_eq!(current_executions, launch_executions,
                "child launch through its first provider turn must not execute the external quoter: {label}");
            assert!(progress.quotation_execution_unchanged,
                "the supplied spec must retain its original quotation after input changes to 42: {current_executions}");
            assert!(!installed.checkpoint);
            assert_eq!(installed.context_parent, None, "selected provider context");
            assert!(installed.acquisition.is_none(),
                "a supplied compiled spec has no source-prepared acquisition receipt: {:?}", installed.acquisition);
            progress.explicit_installers_verified += 1;
            progress.report();
            assert!(installed.tools.iter().any(|tool| matches!(tool,
                exomonad_tool::HostedTool::Function(tool) if tool.name == "probe" && tool.description == "41")));
            round.function("prepared-native-probe", "probe", serde_json::json!({"topic": label}));
            let replied = next_hosted_script_round(&mut requests, &mut pending, origin.actor()).await;
            let output = replied.settled_output("prepared-native-probe");
            assert_eq!(output["status"], "replied", "{output}");
            assert_eq!(output["items"].as_array().unwrap().last().unwrap()["terminalTransfer"], "replyAccepted", "{output}");
            let replied_executions = std::fs::read_to_string(input.with_extension("executions")).unwrap();
            progress.observe_quotations(&prepared_executions, &first_launch_executions, &replied_executions);
            progress.report();
            assert_eq!(replied_executions, launch_executions,
                "child launch through its native reply must not execute the external quoter: {label}");
            assert_eq!(replied_executions, prepared_executions,
                "native replies must preserve the supplied spec's original quotation 41");
            replied.finish();
            progress.observed_native_replies += 1;
            progress.report();
            let row = serde_json::json!({"schema":2,"composition":"engine-store-explicit-prepared-dependency-child",
                "ordinal":ordinal,"actor":child.actor,"provider_origin":origin,"setup_elapsed_ns":elapsed,
                "setup_end":"actual_policy_installed","setup_start":"child_launch_requested",
                "compiler_requests_during_setup":compiler_requests,"installation":details,
                "compiled_installer":installer_details,"compiler_requests_during_installer":installer_compiler_requests,
                "acquisition":installed.acquisition,"prepared_root_acquisition":root_acquisition,
                "prepared_root_completed_inventory_match":true,
                "quotation_executions":{"prepared":prepared_executions.lines().count(),
                    "first_child_launch":first_launch_executions.lines().count(),
                    "parent_before_first_child_launch":progress.parent_quotation_executions,
                    "child_launch":launch_executions.lines().count(),
                    "first_provider_turn":current_executions.lines().count(),
                    "native_reply":replied_executions.lines().count()},
                "native_reply_accepted":true,"producer":endpoint.producer_hex()});
            eprintln!("prepared-runtime-child {row}");
            rows.push(row);
        }
        let completed = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        completed.assert_value("prepared-children", "True");
        progress.parent_result_verified = true;
        completed.finish();
        assert_eq!(children.len(), expected_children);
        assert_eq!(rows.len(), expected_children);
        assert!(progress.root_deployment_original_verified);
        assert_eq!(progress.explicit_installers_verified, expected_children);
        assert_eq!(progress.compiler_requests_during_installers, 0);
        assert_eq!(observations.observations.lock().installer_phases.len(), expected_children);
        assert_eq!(scopes.len(), children.len());
        assert_eq!(std::fs::read_to_string(input.with_extension("executions")).unwrap(), prepared_executions,
            "supplied child specs retain original prepared values without replaying external input42");
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
