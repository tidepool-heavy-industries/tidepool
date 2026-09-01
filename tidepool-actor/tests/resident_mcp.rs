//! Real Haskell `serveTools` policy projected through the generic MCP server.

use std::sync::Arc;
use std::time::Duration;

use tidepool_actor::{
    ActorDescriptor, ActorPlacement, ActorRegistry, ActorWorkbenchSource,
    ExternalApplicationFailure, ExternalApplicationFailureClass, ExternalFailureDisposition,
    ResidentActorDeployment, ResidentActorHost, ResidentActorRoot, ResidentHostParkedKind,
    ResidentLifecyclePolicy,
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
    let declarations = [
        tidepool_mcp::actor_mcp_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::deliberate_decl(),
        tidepool_mcp::fs_read_decl(),
    ];
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
         type ActorEffects = '[ActorMcp, Actor]\n\
         data EchoInput = EchoInput {{ value :: Int }} deriving (Generic, FromJSON, JsonSchema)\n\
         data EchoOutput = EchoOutput {{ doubled :: Int }} deriving (Generic, ToJSON, JsonSchema)\n\
         data SpawnInput = SpawnInput {{ seed :: Int }} deriving (Generic, FromJSON, JsonSchema)\n\
         data SpawnOutput = SpawnOutput {{ started :: Bool }} deriving (Generic, ToJSON, JsonSchema)\n\
         data StateInput = StateInput {{ next :: Int }} deriving (Generic, FromJSON, JsonSchema)\n\
         data StateQuery = StateQuery deriving (Generic, FromJSON, JsonSchema)\n\
         data StateOutput = StateOutput {{ current :: Int }} deriving (Generic, ToJSON, JsonSchema)\n\
         data ResidentTools mode = ResidentTools {{ doubleValue :: mode :- Call EchoInput EchoOutput, spawnChild :: mode :- Call SpawnInput SpawnOutput, currentValue :: mode :- Call StateQuery StateOutput, setValue :: mode :- Update StateInput StateOutput, finishValue :: mode :- Finish StateQuery StateOutput }} deriving (Generic)\n"
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
        ["ActorMcp", "Actor"],
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
    let mut deployments = host
        .take_deployments()
        .expect("take deployment handoff stream");
    assert!(host.take_deployments().is_err());
    let actor = host
        .launch_root(ResidentActorRoot::new(descriptor, machine, outcome))
        .await
        .expect("launch policy actor");
    let report = host
        .run_until_idle()
        .await
        .expect("install resident policy");
    assert_eq!(report.parked[&ResidentHostParkedKind::McpPolicy], 1);

    let ResidentActorDeployment::PolicyInstalled(installation) = deployments
        .try_recv()
        .expect("root policy is handed to deployment")
    else {
        panic!("root policy retired before installation");
    };
    assert_eq!(installation.actor, actor);
    let policy = installation.policy;
    let server = tidepool_mcp::DynamicMcpServer::from_resident_policy(policy)
        .expect("project resident policy into MCP");
    let control = host.control();
    assert_eq!(server.declarations()[0].name, "double_value");
    let (request_shutdown, shutdown_requested) = tokio::sync::oneshot::channel();
    let hosted = tokio::spawn(host.run_until_shutdown(async move {
        let _ = shutdown_requested.await;
    }));

    let calls = [(4, 8), (7, 14)].map(|(input, expected)| {
        let arguments = serde_json::json!({"value": input})
            .as_object()
            .expect("object arguments")
            .clone();
        let server = server.clone();
        async move {
            let result = server
                .dispatch_tool("double_value", arguments)
                .await
                .expect("dispatch resident tool");
            assert_eq!(
                result.structured_content,
                Some(serde_json::json!({"doubled": expected}))
            );
        }
    });
    let [first, second] = calls;
    tokio::join!(first, second);

    let arguments = serde_json::json!({"seed": 11})
        .as_object()
        .expect("object arguments")
        .clone();
    let result = server
        .dispatch_tool("spawn_child", arguments)
        .await
        .expect("dispatch spawning tool");
    assert_eq!(
        result.structured_content,
        Some(serde_json::json!({"started": true}))
    );
    let ResidentActorDeployment::Retired {
        actor: child,
        terminal: child_terminal,
    } = deployments
        .try_recv()
        .expect("a child without an MCP policy still retires")
    else {
        panic!("the completed child unexpectedly installed a policy");
    };
    assert_ne!(child, actor);
    assert_eq!(
        child_terminal.kind,
        tidepool_actor::ActorExitKind::Completed
    );

    let arguments = serde_json::json!({"next": 23})
        .as_object()
        .expect("object arguments")
        .clone();
    let result = server
        .dispatch_tool("set_value", arguments)
        .await
        .expect("dispatch state update");
    assert_eq!(
        result.structured_content,
        Some(serde_json::json!({"current": 23}))
    );
    let result = server
        .dispatch_tool("current_value", serde_json::Map::new())
        .await
        .expect("dispatch state read");
    assert_eq!(
        result.structured_content,
        Some(serde_json::json!({"current": 23}))
    );

    let result = server
        .dispatch_tool("finish_value", serde_json::Map::new())
        .await
        .expect("terminal tool reply settles");
    assert_eq!(
        result.structured_content,
        Some(serde_json::json!({"current": 23}))
    );
    let ResidentActorDeployment::Retired {
        actor: retired,
        terminal,
    } = tokio::time::timeout(Duration::from_secs(1), deployments.recv())
        .await
        .expect("deployment retirement timeout")
        .expect("deployment stream remains open")
    else {
        panic!("policy was installed twice instead of retired");
    };
    assert_eq!(retired, actor);
    assert_eq!(terminal.kind, tidepool_actor::ActorExitKind::Completed);
    assert_eq!(
        control
            .fail_external_application(
                actor,
                ExternalApplicationFailure {
                    class: ExternalApplicationFailureClass::UnexpectedExit,
                    detail: "proxy closed after typed completion".into(),
                },
            )
            .await
            .expect("classify post-terminal application exit"),
        ExternalFailureDisposition::AlreadyTerminal
    );

    request_shutdown.send(()).expect("request host shutdown");
    let shutdown = hosted
        .await
        .expect("host task joins")
        .expect("shutdown policy actor");
    assert!(shutdown.run.failures.is_empty());
    let result = server
        .dispatch_tool("double_value", serde_json::Map::new())
        .await
        .expect("dead actor is an MCP tool failure, not a transport failure");
    assert!(result.is_error.unwrap_or(false));
}
