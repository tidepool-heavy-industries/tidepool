//! Public `startActor` proof through the shared resident runner. Its internal
//! sealing step derives an exact source facade from compiler provenance;
//! startup then runs one typed model/Haskell deliberation, publishes readiness,
//! and resumes the parent with the exact actor incarnation.

use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_actor::{
    ActorDescriptor, ActorLifecycle, ActorMachineRegistry, ActorPlacement, ActorRegistry,
    ActorSourceImports, ActorTurnKind, ActorWorkbenchSource, ResidentActorRunner,
    ResidentActorStarter, ResidentDeliberationExecutor, StartInitiator,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_eval::Value;
use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage};
use tidepool_repr::SessionId;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, ModuleEnv, OutputSink, ResidentOutcome,
    ResidentSession, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness;

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
        request: &Value,
        context: &EffectContext<'_, TestSink>,
    ) -> Result<Option<Response>, EffectError> {
        let _ = (request, context);
        Ok(None)
    }
}

struct ApprovesStartup {
    requests: Mutex<Vec<TurnRequest>>,
}

impl ModelProvider for ApprovesStartup {
    async fn complete(
        &self,
        request: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        self.requests.lock().push(request);
        Ok(TurnResponse {
            text: "```haskell\ncomplete True\n```".into(),
            usage: Usage::default(),
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

#[tokio::test]
async fn public_start_uses_one_exact_resident_path() {
    eval_harness::require_extract();

    let session_id = SessionId(93);
    let decls = [
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::deliberate_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&decls).expect("materialize effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let mut preamble = tidepool_mcp::build_preamble(&decls, false);
    preamble.push_str("type ActorEffects = '[Actor]\n");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().expect("session root");
    let source = r#"
let workerDefinition :: ActorDefinition Int Maybe Int
    workerDefinition =
      ActorDefinition
        "worker"
        (\seed ->
          deliberate "Approve the supplied seed." seed)
        (\seed approved ->
          (pure (if approved then seed + 1 else seed - 1)
            :: Eff '[Deliberate, ActorLocal Maybe Int] Int))
in do
    _ <- startActor workerDefinition 41
    pure (7 :: Int)
"#;
    let compiled = match run_turn(HaskellTurnRequest {
        turn_text: source,
        templates: &templates,
        include: &include_refs,
        session_root: root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .expect("compile public actor program")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("actor program should compile as an expression, got {other:?}"),
    };

    let lib = SessionLib::open(session_id, root.path(), ModuleEnv::standalone_default())
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
    let promotion = machine
        .run_with_sites(
            "start_parent",
            &compiled.expr,
            &compiled.table,
            &compiled.asks,
        )
        .expect("run parent to promotion suspension");

    let machines = Arc::new(ActorMachineRegistry::new());
    assert!(machines.insert_idle(session_id, machine).is_none());
    let registry = ActorRegistry::new();
    let parent_start = registry
        .begin_start(
            None,
            ActorDescriptor::new(
                "parent",
                ["Actor"],
                ActorPlacement {
                    session: session_id,
                    resource_scope: RealmId::fresh(),
                    lexical_scope: ScopeId::ROOT,
                },
            ),
            StartInitiator::Runtime,
        )
        .expect("begin parent");
    let parent = registry
        .publish_ready(parent_start)
        .expect("publish parent");
    let parent_turn = registry
        .begin_turn(parent, ActorTurnKind::Haskell)
        .expect("admit parent Haskell turn");
    let parent_context = parent_turn.session_context();
    let workbench_source = ActorWorkbenchSource::new(preamble, include);
    let runner = ResidentActorRunner::new(Arc::clone(&machines), workbench_source.clone());

    let (start_outcome, facade) = runner
        .promote_definition(parent_context.clone(), promotion)
        .await
        .expect("promote exact actor surface");
    let child_scope = runner
        .mint_isolated_scope(parent_context.clone())
        .await
        .expect("mint isolated child scope");
    let child_realm = RealmId::fresh();
    let start = runner
        .capture_start(parent_context, start_outcome, child_realm)
        .await
        .expect("capture child entry");
    assert_eq!(start.request().label, "worker");
    assert_eq!(start.request().promotion, facade.identity().digest());

    let child_descriptor = ActorDescriptor::new(
        "worker",
        ["Actor", "ActorLocal", "Deliberate"],
        ActorPlacement {
            session: session_id,
            resource_scope: child_realm,
            lexical_scope: child_scope,
        },
    )
    .with_source_imports(ActorSourceImports::from_exact_facades([&facade]));
    let deliberations = ResidentDeliberationExecutor::new(Arc::clone(&machines), workbench_source);
    let starter = ResidentActorStarter::new(registry.clone(), runner, deliberations);
    let provider = ApprovesStartup {
        requests: Mutex::new(Vec::new()),
    };

    let (parent_turn, child, parent_outcome) = starter
        .start(parent_turn, child_descriptor, &provider, start, None)
        .await
        .expect("start promoted actor");
    assert_eq!(parent_turn.kind(), ActorTurnKind::Haskell);
    drop(parent_turn);
    assert_eq!(provider.requests.lock().len(), 1);
    assert_eq!(registry.lifecycle(child), Ok(ActorLifecycle::Exited));
    match parent_outcome {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(7));
        }
        ResidentOutcome::Suspended { .. } => panic!("parent should complete after start"),
    }
}
