use tidepool_testing::eval_harness::{self, EvalHarness};

use exomonad_actor::{
    ActorDescriptor, ActorPlacement, ActorWorkbenchSource, LocalResidentDeployment, ResidentForest,
};
use exomonad_tool::{ToolArguments, ToolInvocation};
use tidepool_bridge::HaskellValue;
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ModuleEnv, OutputSink,
    ResidentSession, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;

use super::support;

#[derive(Clone, Default)]
struct Sink;

impl OutputSink for Sink {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }
    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

struct NoHandlers;

impl DispatchEffect<Sink> for NoHandlers {
    fn dispatch(
        &mut self,
        _: &HaskellValue,
        _: &EffectContext<'_, Sink>,
    ) -> Result<Option<Response>, EffectError> {
        Ok(None)
    }

    fn prepare_dispatch(
        &mut self,
        _: &HaskellValue,
        _: &EffectContext<'_, Sink>,
    ) -> Result<tidepool_effect::dispatch::EffectDispatch, EffectError> {
        Ok(tidepool_effect::dispatch::EffectDispatch::Unhandled)
    }
}

#[test]
fn typed_self_event_sink_compiles_against_generated_actor_local_effect() {
    eval_harness::require_extract();
    let declarations = [
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::actor_kernel_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects module");
    EvalHarness::new()
        .with_stdlib()
        .with_includes(effects.include_paths())
        .compile(include_str!("record_dynamic_sources/compile.hs"), "result")
        .expect("typed dynamic actor source compiles");
}

#[tokio::test]
async fn attached_lifecycle_event_uses_successor_handler_and_actor_cleans_up() {
    run_record_case(include_str!("record_dynamic_sources/run.hs"), 811).await;
}

#[tokio::test]
async fn failed_handler_retains_attached_source_through_replacement() {
    run_record_case(
        include_str!("record_dynamic_sources/run_failed_handler.hs"),
        812,
    )
    .await;
}

async fn run_record_case(source: &str, discriminator: u32) {
    eval_harness::require_extract();
    let session = support::process_unique_session(discriminator);
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        "Tidepool.Agent.Contract",
    );
    let preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Actor.Record as R");
    let preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Actor as Actor");
    let preamble = insert_preamble_imports(&preamble, "Tidepool.Effects.Row (knownEffects)");
    let preamble = format!(
        "{preamble}{}",
        include_str!("record_dynamic_sources/schema.hs")
    );
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let templates = [templates[3].clone(), templates[5].clone()];
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let session_root = tempfile::tempdir().unwrap();
    let lib = SessionLib::open(
        session,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(include.clone());
    let mut machine =
        ResidentSession::unbootstrapped(NoHandlers, Sink, DEFAULT_NURSERY_SIZE, Some(lib));
    let retained = machine.prepared_retained();
    let compiled = match tidepool_testing::with_settlement(|settlement| {
        run_turn(
            HaskellTurnRequest {
                exact_context: None,
                session_id: None,
                turn_text: source,
                templates: &templates,
                include: &include_refs,
                session_root: session_root.path(),
                inject_modules: &[],
                gen: 1,
                verdict: None,
                target: None,
                retained_imports: &retained,
            },
            settlement,
        )
    })
    .expect("compile lifecycle watcher")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("watcher should be an expression, got {other:?}"),
    };
    machine.set_effect_execution(
        EffectRunPolicy::SuspendAll,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
    let outcome = tidepool_testing::with_settlement(|settlement| {
        machine.run_with_sites("record_dynamic_sources", compiled.code(), settlement)
    })
    .expect("first actor boundary");
    let descriptor = ActorDescriptor::new(
        "record-dynamic-sources",
        ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope: ScopeId::ROOT,
        },
    );
    let (forest, mut deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        None,
        exomonad_actor::Incarnation::FIRST,
    );
    let (actor, task) = forest.admit_root(descriptor, outcome).await.unwrap();
    let LocalResidentDeployment::PolicyInstalled(installation) = deployments.recv().await.unwrap()
    else {
        panic!("root retired before installing the test policy");
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        installation.policy.dispatch_boxed(ToolInvocation {
            context: None,
            name: "run_case".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        }),
    )
    .await
    .expect("dynamic lifecycle case timed out")
    .expect("dynamic lifecycle case")
    .into_json()
    .expect("resident response serializes for structured test assertions");
    assert_eq!(
        result.get("passed"),
        Some(&serde_json::json!(true)),
        "{result}"
    );
    tokio::time::timeout(std::time::Duration::from_secs(20), task)
        .await
        .expect("root actor task timed out")
        .expect("root actor task");
    assert!(actor.terminal().cleanup().is_some());
}
