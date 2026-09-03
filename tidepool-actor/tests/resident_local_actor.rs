//! The real resident MCP policy driven directly by the canonical local actor.

use std::sync::Arc;

use tidepool_actor::{
    spawn_resident_root, ActorDescriptor, ActorPlacement, ActorWorkbenchSource,
    LocalResidentDeployment, ResidentActorRoot,
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
        panic!("resident MCP fixture must not open a model round")
    }
}

#[tokio::test]
async fn local_actor_owns_resident_policy_children_and_terminal_reply() {
    eval_harness::require_extract();

    let session = support::process_unique_session(177);
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::deliberate_decl(),
        tidepool_mcp::fs_read_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        "Tidepool.Agent.Contract",
    );
    let preamble = format!(
        "{preamble}\
         type ActorEffects = '[AgentTools, Actor]\n\
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
        turn_text: include_str!("resident_local_actor/policy.hs"),
        templates: &templates,
        include: &include_refs,
        session_root: session_root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .expect("compile resident policy")
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
        .run_with_sites(
            "resident_local_policy",
            &compiled.expr,
            &compiled.table,
            &compiled.asks,
        )
        .expect("first policy boundary");
    let descriptor = ActorDescriptor::new(
        "resident-local-policy",
        ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope: ScopeId::ROOT,
        },
    );
    let (actor, task, mut deployments) = spawn_resident_root(
        ActorWorkbenchSource::new(preamble, include),
        Arc::new(NoModelRounds),
        None,
        ResidentActorRoot::new(descriptor, machine, outcome),
    )
    .await
    .expect("spawn local resident root");

    let LocalResidentDeployment::PolicyInstalled(installation) =
        deployments.recv().await.expect("policy installation")
    else {
        panic!("root retired before installing policy");
    };
    assert_eq!(installation.actor.identity(), actor.identity());
    let server = tidepool_mcp::DynamicMcpServer::from_resident_policy(installation.policy)
        .expect("MCP projection");
    let arguments = serde_json::json!({"value": 6})
        .as_object()
        .expect("arguments")
        .clone();
    let doubled = server
        .dispatch_tool("double_value", arguments)
        .await
        .expect("double value");
    assert_eq!(
        doubled.structured_content,
        Some(serde_json::json!({"doubled": 12}))
    );

    let spawned = server
        .dispatch_tool(
            "spawn_child",
            serde_json::json!({"seed": 19})
                .as_object()
                .expect("arguments")
                .clone(),
        )
        .await
        .expect("spawn and await child");
    assert!(!spawned.is_error.unwrap_or(false), "{spawned:?}");
    assert_eq!(
        spawned.structured_content,
        Some(serde_json::json!({"started": true}))
    );
    let child_retired = deployments.recv().await.expect("child retirement");
    assert!(matches!(
        child_retired,
        LocalResidentDeployment::Retired { ref terminal, .. }
            if terminal.kind == tidepool_actor::ActorExitKind::Completed
    ));
    assert!(matches!(
        deployments.recv().await,
        Some(LocalResidentDeployment::ChildExited { ref notice, .. })
            if notice.terminal.kind == tidepool_actor::ActorExitKind::Completed
    ));

    let finished = server
        .dispatch_tool("finish_value", serde_json::Map::new())
        .await
        .expect("finish value");
    assert_eq!(
        finished.structured_content,
        Some(serde_json::json!({"current": 0}))
    );
    task.await.expect("root actor task");
    assert_eq!(
        actor.terminal().wait().await.kind,
        tidepool_actor::ActorExitKind::Completed
    );
    assert!(matches!(
        deployments.recv().await,
        Some(LocalResidentDeployment::Retired { actor: retired, .. })
            if retired == actor.identity()
    ));
}
