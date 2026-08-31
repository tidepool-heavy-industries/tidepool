//! Adversarial proof that start sealing rejects a later same-spelled nominal
//! head instead of pairing it with an already-rooted actor definition.

use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_actor::{
    ActorAgentSession, ActorDescriptor, ActorEvent, ActorMachineRegistry, ActorPlacement,
    ActorRegistry, ActorStartCaptureError, ActorTurnKind, ActorWorkbenchSource,
    ResidentActorRunner, ResidentCompletionExecutor, StartInitiator,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_eval::Value;
use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage};
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, ModuleEnv, OutputSink, ResidentSession, SessionLib,
    TurnRequest as HaskellTurnRequest, TurnResult,
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
        request: &Value,
        context: &EffectContext<'_, TestSink>,
    ) -> Result<Option<Response>, EffectError> {
        let _ = (request, context);
        Ok(None)
    }
}

struct DefinesThenShadows {
    requests: Mutex<usize>,
}

impl ModelProvider for DefinesThenShadows {
    async fn complete(
        &self,
        request: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let mut requests = self.requests.lock();
        let ordinal = *requests;
        *requests += 1;
        Ok(TurnResponse {
            text: match ordinal {
                0 => include_str!("sealing_shadow_drift/provider_response.hs"),
                1 => include_str!("sealing_shadow_drift/redefinition_response.hs"),
                _ => panic!(
                    "sealing must fail before child startup prompts: {:#?}",
                    request.messages
                ),
            }
            .trim_end()
            .into(),
            usage: Usage::default(),
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

#[tokio::test]
async fn start_rejects_a_shadowed_selected_head_before_allocation() {
    eval_harness::require_extract();

    let session_id = support::process_unique_session(94);
    let decls = [
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::deliberate_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&decls).expect("materialize effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let mut preamble = tidepool_mcp::build_preamble(&decls, false);
    preamble.push_str("type ActorEffects = '[Actor, Deliberate]\n");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().expect("session root");
    let compiled = match run_turn(HaskellTurnRequest {
        turn_text: include_str!("sealing_shadow_drift/parent_program.hs"),
        templates: &templates,
        include: &include_refs,
        session_root: root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .expect("compile shadow-drift parent")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("parent should compile as an expression, got {other:?}"),
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
    let parent_outcome = machine
        .run_with_sites(
            "shadow_drift_parent",
            &compiled.expr,
            &compiled.table,
            &compiled.asks,
        )
        .expect("run parent to definition prompt");

    let machines = Arc::new(ActorMachineRegistry::new());
    assert!(machines.insert_idle(session_id, machine).is_none());
    let registry = ActorRegistry::new();
    let parent_realm = RealmId::fresh();
    let starting = registry
        .begin_start(
            None,
            ActorDescriptor::new(
                "parent",
                ["Actor", "Deliberate"],
                ActorPlacement {
                    session: session_id,
                    resource_scope: parent_realm,
                    lexical_scope: ScopeId::ROOT,
                },
            ),
            StartInitiator::Runtime,
        )
        .expect("begin parent");
    let parent = registry.publish_ready(starting).expect("publish parent");
    let parent_turn = registry
        .begin_turn(parent, ActorTurnKind::Haskell)
        .expect("admit parent turn");
    let source = ActorWorkbenchSource::new(preamble, include);
    let runner = ResidentActorRunner::new(Arc::clone(&machines), source.clone());
    let completion = runner
        .capture_completion(parent_turn.session_context(), parent_outcome, parent_realm)
        .await
        .expect("capture actor-definition obligation");
    let provider = DefinesThenShadows {
        requests: Mutex::new(0),
    };
    let agent = ActorAgentSession::attach(registry.clone(), parent).expect("attach parent model");
    let completions = ResidentCompletionExecutor::new(Arc::clone(&machines), source);
    let (parent_turn, redefinition_outcome) = completions
        .resolve(&agent, parent_turn, &provider, completion, None)
        .await
        .expect("author and root the definition before shadowing");
    let redefinition = runner
        .capture_completion(
            parent_turn.session_context(),
            redefinition_outcome,
            parent_realm,
        )
        .await
        .expect("capture the in-program redefinition obligation");
    let (parent_turn, start_outcome) = completions
        .resolve(&agent, parent_turn, &provider, redefinition, None)
        .await
        .expect("redefine the selected head before resuming startActor");

    let error = match runner
        .capture_start(parent_turn.session_context(), start_outcome)
        .await
    {
        Ok(_) => panic!("the sealing membrane accepted shadow drift"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        tidepool_actor::ResidentActorWorkbenchError::StartCapture(
            ActorStartCaptureError::ShadowDrift { ref head, .. }
        ) if head == "DriftStartup"
    ));
    assert_eq!(*provider.requests.lock(), 2);
    assert_eq!(
        registry
            .events()
            .into_iter()
            .filter(|record| matches!(record.event, ActorEvent::Created { .. }))
            .count(),
        1,
        "sealing fails before allocating a child identity"
    );
}
