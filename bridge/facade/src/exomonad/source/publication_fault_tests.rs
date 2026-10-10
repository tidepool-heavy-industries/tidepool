//! Real source publication, actor admission, and restart under an isolated fsync fault.

#![allow(
    clippy::disallowed_methods,
    reason = "test-only isolated fault children compile the existing syscall fixture and relaunch this test executable"
)]

use super::*;
use crate::transport_test_support::ResidentToolEndpointTestExt;
use exomonad_actor::{
    ActorDescriptor, ActorPlacement, ActorSourceLayers, ActorWorkbenchSource,
    LocalResidentDeployment, ResidentForest, WorkbenchCancellationOutcome,
};
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tidepool_bridge::HaskellValue;
use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
use tidepool_effect::{
    dispatch::{DispatchEffect, EffectContext},
    error::EffectError,
    EffectRunPolicy, LivePayloadPolicy, Response,
};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ModuleEnv, ResidentSession,
    SessionLib, TurnRequest, TurnResult,
};

const CHILD_TEST: &str = "exomonad::source::publication_fault_tests::publication_fault_child";
const DIRECTORY_FAULT: &str =
    include_str!("../../../../atomic-write/tests/fixtures/directory_fault.c");

fn spec_source() -> String {
    tidepool_testing::fixture_source("bridge/facade/src/actor_host/fixtures/browser_agent_spec.hs")
        .replace(
            "module AgentSpec (agentSpec)",
            "module AgentSpec (agentSpec, Probe (..), answer)",
        )
}

struct NoHandlers;

impl DispatchEffect<tidepool_mcp::CapturedOutput> for NoHandlers {
    fn dispatch(
        &mut self,
        _: &HaskellValue,
        _: &EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> std::result::Result<Option<Response>, EffectError> {
        Ok(None)
    }

    fn prepare_dispatch(
        &mut self,
        _: &HaskellValue,
        _: &EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> std::result::Result<tidepool_effect::dispatch::EffectDispatch, EffectError> {
        Ok(tidepool_effect::dispatch::EffectDispatch::Unhandled)
    }
}

fn source_owner(project: &Path, run: &Path) -> ExomonadSourceReload {
    ExomonadSourceReload::new(
        FrozenWorkspace::load(project, run).unwrap(),
        project.to_path_buf(),
        run.to_path_buf(),
        crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
    )
}

fn invocation(key: &str, name: &str, arguments: serde_json::Value) -> ToolInvocation {
    ToolInvocation {
        context: Some(context(key)),
        name: name.to_owned(),
        arguments: ToolArguments::Structured(arguments),
    }
}

fn context(key: &str) -> ToolInvocationContext {
    ToolInvocationContext::external(
        "source-publication-fault".into(),
        key.into(),
        key.into(),
        Some(key.into()),
        None,
    )
}

async fn actor_case(project: &Path, run: &Path, fault: bool) {
    tidepool_testing::eval_harness::require_extract();
    let layers = Arc::new(source_owner(project, run));
    let declarations = [
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::actor_kernel_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut include = effects.include_paths().to_vec();
    include.push(tidepool_testing::eval_harness::prelude_path());
    include.extend(layers.layer.active_include_paths().unwrap());
    include.extend(layers.frozen.include.iter().cloned());
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_notebook_preamble(&declarations, false),
        "PublicationFaultDriver (FaultEffects, faultDriver)",
    );
    let preamble = insert_preamble_imports(&preamble, "Tidepool.Agent.Contract");
    let session = tidepool_repr::SessionId((u64::from(std::process::id()) << 32) | 194);
    let session_root = tempfile::tempdir().unwrap();
    let lib = SessionLib::open(
        session,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(include.clone());
    let mut machine = ResidentSession::unbootstrapped(
        NoHandlers,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let templates = resident_workbench_templates(&preamble, "FaultEffects", "");
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let compiled = match tidepool_testing::with_settlement(|settlement| {
        run_turn(
            TurnRequest {
                exact_context: None,
                session_id: None,
                turn_text: "faultDriver",
                templates: &templates,
                include: &include_refs,
                session_root: session_root.path(),
                inject_modules: &[],
                gen: 1,
                verdict: None,
                target: None,
                retained_imports: &[],
            },
            settlement,
        )
    })
    .expect("compile the real agent attachment and mailbox driver")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("expected expression driver, got {other:?}"),
    };
    let resource_scope = RealmId::fresh();
    machine
        .set_actor_execution(
            tidepool_runtime::session::SessionRunContext {
                resource_scope,
                lexical_scope: ScopeId::ROOT,
                ..tidepool_runtime::session::SessionRunContext::ROOT
            },
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        )
        .unwrap();
    let outcome = machine
        .run_with_sites("source_publication_fault", compiled.code())
        .unwrap();
    let (mut forest, mut deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include).with_spec("AgentSpec.agentSpec"),
        session,
        machine,
        None,
        exomonad_actor::Incarnation::FIRST,
    );
    forest.set_source_layers(layers.clone());
    let (actor, task) = forest
        .admit_root(
            ActorDescriptor::new(
                "source-publication-fault",
                ActorPlacement {
                    session,
                    resource_scope,
                    lexical_scope: ScopeId::ROOT,
                },
            )
            .with_capabilities(
                exomonad_actor::ActorCapabilities::default().with_effect_keys(Vec::new()),
            ),
            outcome,
        )
        .await
        .unwrap();
    let LocalResidentDeployment::PolicyInstalled(installation) =
        tokio::time::timeout(Duration::from_secs(180), deployments.recv())
            .await
            .expect("agent spec installs")
            .expect("deployment channel remains live")
    else {
        panic!("actor retired before installing its actual spec");
    };
    layers.bind_run(tidepool_repr::PrincipalId::from(actor.identity()));
    let policy = installation.policy;
    let initial = policy
        .dispatch_json_boxed(invocation(
            "initial-probe",
            "probe",
            serde_json::json!({"number": 40}),
        ))
        .await
        .unwrap();
    assert_eq!(initial["status"], "committed", "{initial}");
    assert_eq!(
        initial["items"][0]["output"],
        if fault { "Number 42" } else { "Number 240" },
        "the real spec implementation runs through the interactive workbench"
    );
    if fault {
        let old_request = policy
            .snapshot_for_request()
            .expect("old accepted handler/source snapshot");
        let before = layers.layer.read_active().unwrap().unwrap();
        let old_link = std::fs::read_link(layers.layer.active_link()).unwrap();
        let initial_spec = spec_source();
        let updated = initial_spec.replace("value + 2", "value + 200");
        assert_ne!(updated, initial_spec);
        std::fs::write(project.join(".exomonad/AgentSpec.hs"), updated).unwrap();
        let failure = policy
            .dispatch_json_boxed(invocation(
                "uncertain-reload",
                "reload_agent_spec",
                serde_json::json!({}),
            ))
            .await
            .expect_err("visible pair and uncertain durability cannot become a committed reload");
        let detail = failure.to_string();
        assert!(
            detail.contains("durability is unconfirmed")
                && detail.contains("paired source revision")
                && detail.contains("install 2"),
            "{detail}"
        );
        let new_link = std::fs::read_link(layers.layer.active_link()).unwrap();
        assert_ne!(
            new_link, old_link,
            "the real source symlink rename is visible"
        );
        let repair = layers
            .layer
            .read_active()
            .expect_err("the real side-record repair also encounters the parent fsync fault");
        assert!(!repair.to_string().is_empty());
        // The repair's side-record rename is visible even though that separate
        // durability operation also failed. It never confirms the reload claim.
        let visible = layers.layer.read_active().unwrap().unwrap();
        assert_ne!(visible.identity, before.identity);
        assert_eq!(visible.generation, before.generation + 1);
        let recovered_spec = std::fs::read_to_string(
            layers
                .layer
                .revisions()
                .join(&visible.identity)
                .join("0/AgentSpec.hs"),
        )
        .unwrap();
        assert!(recovered_spec.contains("value + 200"));
        let status = policy
            .dispatch_json_boxed(invocation(
                "uncertain-status",
                "status",
                serde_json::json!({"view": "detailed"}),
            ))
            .await
            .expect("builtin status observes the installed pair after durability failure");
        assert!(
            status["items"][0]["output"]
                .as_str()
                .unwrap()
                .contains("install=2"),
            "{status}"
        );
        let new_probe = policy
            .dispatch_json_boxed(invocation(
                "new-visible-probe",
                "probe",
                serde_json::json!({"number": 40}),
            ))
            .await
            .unwrap();
        assert_eq!(
            new_probe["items"][0]["output"], "Number 240",
            "new handlers are active despite durability uncertainty"
        );
        let old_probe = old_request
            .dispatch_json_boxed(invocation(
                "old-accepted-probe",
                "probe",
                serde_json::json!({"number": 40}),
            ))
            .await
            .unwrap();
        assert_eq!(
            old_probe["items"][0]["output"], "Number 42",
            "the old accepted snapshot retains its original handlers"
        );
        let source_cell = policy.dispatch_json_boxed(ToolInvocation {
            context: Some(context("new-visible-source")),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw("import qualified AgentSpec\nAgentSpec.answer (AgentSpec.Probe 40) >>= inspectFull".into()),
        }).await.unwrap();
        assert!(
            source_cell["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["output"] == "240"),
            "new source is active together with its handlers: {source_cell}"
        );
        assert!(
            matches!(
                policy
                    .cancel_workbench_boxed(context("uncertain-reload"))
                    .await
                    .unwrap(),
                WorkbenchCancellationOutcome::Unconfirmed { .. }
            ),
            "the actual post-rename publication cannot authorize replay"
        );
        std::fs::write(
            run.join("visible-publication.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "identity": visible.identity,
                "generation": visible.generation,
                "reload_failure": detail,
                "original_outcome": "Unconfirmed",
                "actor_pid": std::process::id(),
            }))
            .unwrap(),
        )
        .unwrap();
    } else {
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(run.join("visible-publication.json")).unwrap())
                .unwrap();
        assert_ne!(report["actor_pid"], serde_json::json!(std::process::id()));
        let visible = layers.layer.read_active().unwrap().unwrap();
        assert_eq!(visible.identity, report["identity"]);
        assert_eq!(visible.generation, report["generation"]);
        let source = layers
            .freeze_checkpoint_layer(tidepool_repr::PrincipalId::SYSTEM)
            .unwrap();
        assert!(source
            .identities()
            .contains(&format!("run:{}", visible.identity)));
        assert_eq!(
            report["original_outcome"], "Unconfirmed",
            "fresh installation does not reconcile the original process's publication outcome"
        );
        std::fs::write(
            run.join("fresh-process-recovery.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "identity": visible.identity,
                "generation": visible.generation,
                "original_actor_pid": report["actor_pid"],
                "fresh_actor_pid": std::process::id(),
                "original_outcome": report["original_outcome"],
                "probe_output": initial["items"][0]["output"],
                "frozen_identities": source.identities(),
            }))
            .unwrap(),
        )
        .unwrap();
    }
    forest.shutdown().await;
    assert!(actor
        .terminal()
        .cleanup()
        .expect("retained actor cleanup")
        .is_confirmed());
    tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("actor task exits after confirmed shutdown")
        .unwrap();
}

#[test]
#[ignore = "only invoked by the isolated syscall-fault outer test"]
fn publication_fault_child() {
    let project = PathBuf::from(
        std::env::var_os("PUBLICATION_FAULT_PROJECT").expect("isolated child project"),
    );
    let run = PathBuf::from(std::env::var_os("PUBLICATION_FAULT_RUN").expect("isolated child run"));
    let phase = std::env::var("PUBLICATION_FAULT_PHASE").unwrap();
    assert!(matches!(phase.as_str(), "fault" | "recover"));
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(actor_case(&project, &run, phase == "fault"));
}

fn child(mut command: Command, phase: &str, run: &Path) -> std::result::Result<(), String> {
    let stdout = run.join(format!("{phase}-stdout.log"));
    let stderr = run.join(format!("{phase}-stderr.log"));
    let mut child = command
        .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()))
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(900) {
            child.kill().unwrap();
            child.wait().unwrap();
            return Err(format!("{phase} child exceeded 900s"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = child.wait().unwrap();
    let output = std::fs::read_to_string(&stdout).unwrap();
    let errors = std::fs::read_to_string(&stderr).unwrap();
    if !status.success() {
        return Err(format!(
            "{phase} child failed ({status}): {output} {errors}"
        ));
    }
    assert!(
        output.contains("1 passed; 0 failed"),
        "{phase} selected no child test: {output}"
    );
    println!("isolated {phase} child executed one test: {output}");
    Ok(())
}

#[test]
fn postrename_fsync_failure_keeps_paired_spec_and_fresh_process_recovers_visible_source() {
    let fixture_root = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT");
    let fixture = |prefix: &str| {
        let mut builder = tempfile::Builder::new();
        builder.prefix(prefix);
        match fixture_root.as_ref() {
            Some(root) => builder.tempdir_in(root).unwrap(),
            None => builder.tempdir().unwrap(),
        }
    };
    let project = fixture("publication-fault-project-");
    let run = fixture("publication-fault-run-");
    let authored = project.path().join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(authored.join("config.toml"), "[defaults]\nmodel='gpt-6-sol'\n[haskell]\nsource_roots=['.']\nmodules=['PublicationFaultDriver']\nspec='AgentSpec.agentSpec'\n").unwrap();
    std::fs::write(authored.join("AgentSpec.hs"), spec_source()).unwrap();
    std::fs::write(
        authored.join("PublicationFaultDriver.hs"),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/exomonad/source/publication_fault_driver.hs",
        ),
    )
    .unwrap();
    {
        let owner = source_owner(project.path(), run.path());
        owner.layer.ensure_active(&owner.frozen).unwrap();
        owner.ensure_helper_active("run").unwrap();
    }
    let source = run.path().join("directory_fault.c");
    let library = run.path().join("directory_fault.so");
    std::fs::write(&source, DIRECTORY_FAULT).unwrap();
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .arg("-ldl")
        .status()
        .unwrap()
        .success());
    let hits = run.path().join("directory-fault-hits");
    for phase in ["fault", "recover"] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", CHILD_TEST, "--nocapture"])
            .env("PUBLICATION_FAULT_PROJECT", project.path())
            .env("PUBLICATION_FAULT_RUN", run.path())
            .env("PUBLICATION_FAULT_PHASE", phase);
        if phase == "fault" {
            command
                .env("LD_PRELOAD", &library)
                .env("FAULT_PATH", SourceLayer::new(run.path()).directory)
                .env("FAULT_KIND", "sync")
                .env("FAULT_LOG", &hits);
        } else {
            command
                .env_remove("LD_PRELOAD")
                .env_remove("FAULT_PATH")
                .env_remove("FAULT_KIND")
                .env_remove("FAULT_LOG");
        }
        if let Err(failure) = child(command, phase, run.path()) {
            let retained_project = project.keep();
            let retained_run = run.keep();
            panic!("{failure}; retained project {retained_project:?}, run {retained_run:?}");
        }
        if phase == "fault" {
            let failures = std::fs::read_to_string(&hits).unwrap().lines().count();
            assert!(failures >= 2,
                "real parent-directory fsync must fail both publication confirmation and explicit side-record repair");
            println!("actual parent-directory fsync failures: {failures}");
        }
    }
    if fixture_root.is_some() {
        let retained_project = project.keep();
        let retained_run = run.keep();
        println!("retained successful fixture project {retained_project:?}, run {retained_run:?}");
    }
}
