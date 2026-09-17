//! Structured introspection must traverse the real resident Eff boundary.

use tidepool_actor::{
    ActorDescriptor, ActorPlacement, ActorWorkbenchSource, LocalResidentDeployment, ResidentForest,
};
use tidepool_bridge::Value;
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ModuleEnv, OutputSink,
    PreparedTurn, ResidentSession, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness;
use tidepool_tool::{ToolArguments, ToolInvocation};

mod support;

#[derive(Clone, Default)]
struct TestSink;

impl OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }

    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

struct NoHandlers;

impl DispatchEffect<TestSink> for NoHandlers {
    fn dispatch(
        &mut self,
        _request: &Value,
        _context: &EffectContext<'_, TestSink>,
    ) -> Result<Option<Response>, EffectError> {
        Ok(None)
    }
}

#[tokio::test]
async fn resident_eff_structured_introspection_is_reentrant_and_read_only() {
    eval_harness::require_extract();

    let session = support::process_unique_session(189);
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::introspection_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        "Tidepool.Agent.Contract",
    );
    let preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Introspection as API");
    let preamble = format!(
        "{preamble}\
         type ActorEffects = '[AgentTools, Actor, Introspection]\n\
         data InspectInput = InspectInput deriving (Generic, FromJSON, JsonSchema)\n\
         data InspectOutput = InspectOutput {{ localName :: Text, localFields :: [Text], functionType :: Text, publicName :: Text, unknownTyped :: Bool, ambiguousTyped :: Bool, namespaceTyped :: Bool, unknownModuleTyped :: Bool, inspectionGeneration :: Int, inspectionFingerprint :: Text }} deriving (Generic, ToJSON, JsonSchema)\n\
         data FinishInput = FinishInput deriving (Generic, FromJSON, JsonSchema)\n\
         data FinishOutput = FinishOutput {{ finished :: Bool }} deriving (Generic, ToJSON, JsonSchema)\n\
         data InspectionTools mode = InspectionTools {{ inspectApi :: mode :- Call InspectInput InspectOutput, finishInspection :: mode :- Finish FinishInput FinishOutput }} deriving (Generic)\n"
    );
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let session_root = tempfile::tempdir().expect("session root");
    let compiled = match run_turn(HaskellTurnRequest {
        turn_text: include_str!("resident_introspection/policy.hs"),
        templates: &templates,
        include: &include_refs,
        session_root: session_root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
        prepared: PreparedTurn::first_turn(),
    })
    .expect("compile resident introspection policy")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("policy should be an expression, got {other:?}"),
    };
    let lib = SessionLib::open(
        session,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("declaration plane")
    .with_validation_include(include.clone());
    let mut machine = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        NoHandlers,
        TestSink,
        include.clone(),
        DEFAULT_NURSERY_SIZE,
        Some(lib),
    )
    .expect("resident machine");
    machine.set_effect_execution(
        EffectRunPolicy::SuspendAll,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
    let outcome = machine
        .run_with_sites("resident_introspection_policy", compiled.code())
        .expect("policy boundary");
    let descriptor = ActorDescriptor::new(
        "resident-introspection",
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
        tidepool_actor::Incarnation::FIRST,
    );
    let (actor, task) = forest
        .admit_root(descriptor, outcome)
        .await
        .expect("admit resident introspection actor");
    let LocalResidentDeployment::PolicyInstalled(installation) =
        deployments.recv().await.expect("policy installation")
    else {
        panic!("actor retired before installing policy")
    };
    let policy = installation.policy;

    let first = policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "inspect_api".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        })
        .await
        .expect("inspect through resident Eff");
    let second = policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "inspect_api".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        })
        .await
        .expect("repeat inspection");
    assert_eq!(first, second, "inspection must not advance the scope");
    assert_eq!(first["localName"], "InspectOutput");
    assert_eq!(
        first["localFields"],
        serde_json::json!([
            "localName",
            "localFields",
            "functionType",
            "publicName",
            "unknownTyped",
            "ambiguousTyped",
            "namespaceTyped",
            "unknownModuleTyped",
            "inspectionGeneration",
            "inspectionFingerprint"
        ])
    );
    assert!(first["functionType"]
        .as_str()
        .is_some_and(|signature| signature.contains("Eff")));
    assert_eq!(first["publicName"], "ActorDefinition");
    assert_eq!(first["unknownTyped"], true);
    assert_eq!(first["ambiguousTyped"], true);
    assert_eq!(first["namespaceTyped"], true);
    assert_eq!(first["unknownModuleTyped"], true);
    assert!(first["inspectionGeneration"].as_i64().is_some());
    assert_eq!(
        first["inspectionFingerprint"].as_str().map(str::len),
        Some(64)
    );

    policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "finish_inspection".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        })
        .await
        .expect("finish actor");
    task.await.expect("actor task");
    assert_eq!(
        actor.terminal().wait().await.kind,
        tidepool_actor::ActorExitKind::Completed
    );
}
