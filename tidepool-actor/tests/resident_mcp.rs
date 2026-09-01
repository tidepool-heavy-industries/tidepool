//! Real Haskell `serveTools` policy projected through the generic MCP server.

use std::sync::Arc;
use std::time::Duration;

use tidepool_actor::{
    ActorDescriptor, ActorPlacement, ActorRegistry, ActorWorkbenchSource, ResidentActorHost,
    ResidentActorRoot, ResidentHostParkedKind, ResidentLifecyclePolicy,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_eval::Value;
use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ModuleEnv, OutputSink,
    ResidentSession, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness;

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

struct NoModelRounds;

impl ModelProvider for NoModelRounds {
    async fn complete(
        &self,
        _request: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        panic!("a resident MCP policy must not open a model round")
    }
}

#[tokio::test]
async fn resident_policy_serves_repeated_typed_haskell_calls_and_dies_with_its_actor() {
    eval_harness::require_extract();

    let session = support::process_unique_session(117);
    let declarations = [tidepool_mcp::actor_mcp_decl()];
    let effects = tidepool_mcp::ensure_effects_module(&declarations)
        .expect("materialize ActorMcp effect module");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        "Tidepool.Agent.Contract",
    );
    let preamble = format!(
        "{preamble}\
         type ActorEffects = '[ActorMcp]\n\
         data EchoInput = EchoInput {{ value :: Int }} deriving (Generic, FromJSON, JsonSchema)\n\
         data EchoOutput = EchoOutput {{ doubled :: Int }} deriving (Generic, ToJSON)\n\
         data ResidentTools mode = ResidentTools {{ doubleValue :: mode :- Call EchoInput EchoOutput }} deriving (Generic)\n"
    );
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let session_root = tempfile::tempdir().expect("session root");
    let compiled = match run_turn(HaskellTurnRequest {
        turn_text: include_str!("resident_mcp/policy.hs"),
        templates: &templates,
        include: &include_refs,
        session_root: session_root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .expect("compile resident MCP policy")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("policy should compile as an expression, got {other:?}"),
    };
    let lib = SessionLib::open(
        session,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("open declaration plane")
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
    .expect("bootstrap resident machine");
    machine.set_effect_execution(
        EffectRunPolicy::SuspendAll,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
    let outcome = machine
        .run_with_sites(
            "resident_mcp_policy",
            &compiled.expr,
            &compiled.table,
            &compiled.asks,
        )
        .expect("run policy to its first await");
    let descriptor = ActorDescriptor::new(
        "resident MCP policy",
        ["ActorMcp"],
        ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope: ScopeId::ROOT,
        },
    );
    let mut host = ResidentActorHost::new(
        ActorRegistry::new(),
        ActorWorkbenchSource::new(preamble, include),
        Arc::new(NoModelRounds),
        None,
        ResidentLifecyclePolicy::new(Duration::ZERO),
    )
    .expect("construct actor host");
    let actor = host
        .launch_root(ResidentActorRoot::new(descriptor, machine, outcome))
        .await
        .expect("launch policy actor");
    let report = host
        .run_until_idle()
        .await
        .expect("install resident policy");
    assert_eq!(report.parked[&ResidentHostParkedKind::McpPolicy], 1);

    let policy = host.mcp_policy(actor).expect("installed actor policy");
    let server = tidepool_mcp::DynamicMcpServer::from_resident_policy(policy)
        .expect("project resident policy into MCP");
    assert_eq!(server.declarations()[0].name, "double_value");
    for (input, expected) in [(4, 8), (7, 14)] {
        let arguments = serde_json::json!({"value": input})
            .as_object()
            .expect("object arguments")
            .clone();
        let result = server
            .dispatch_tool("double_value", arguments)
            .await
            .expect("dispatch resident tool");
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({"doubled": expected}))
        );
    }

    host.shutdown().await.expect("shutdown policy actor");
    let result = server
        .dispatch_tool("double_value", serde_json::Map::new())
        .await
        .expect("dead actor is an MCP tool failure, not a transport failure");
    assert!(result.is_error.unwrap_or(false));
}
