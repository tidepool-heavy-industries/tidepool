//! Public `deliberate` effect -> resident model/Haskell session -> exact
//! continuation resumption. This is the Stage-2/Stage-3 seam exercised as one
//! GHC/JIT vertical rather than independent request and workbench tests.

use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_actor::{
    ActorAgentSession, ActorDescriptor, ActorMachineRegistry, ActorPlacement, ActorRegistry,
    ActorTurnKind, ActorWorkbenchSource, ResidentCompletion, ResidentCompletionExecutor,
    StartInitiator,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::Value;
use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage};
use tidepool_runtime::session::registry::CheckoutRequest;
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ModuleEnv, OutputSink,
    ResidentOutcome, ResidentSession, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
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

struct CompletesFromMountedInput {
    requests: Mutex<Vec<TurnRequest>>,
}

impl ModelProvider for CompletesFromMountedInput {
    async fn complete(
        &self,
        request: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        assert_eq!(
            request
                .messages
                .last()
                .map(|message| message.content.as_str()),
            Some("Check whether the supplied integer is forty-one.")
        );
        self.requests.lock().push(request);
        Ok(TurnResponse {
            text: include_str!("resident_deliberate/completion_response.hs")
                .trim_end()
                .into(),
            usage: Usage::default(),
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

#[tokio::test]
async fn public_deliberate_mounts_live_input_and_resumes_its_exact_continuation() {
    eval_harness::require_extract();

    let session_id = support::process_unique_session(92);
    let actor_realm = RealmId::fresh();
    let effects = tidepool_mcp::ensure_effects_module(&[tidepool_mcp::deliberate_decl()])
        .expect("materialize Deliberate effect module");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let mut preamble = tidepool_mcp::build_preamble(&[tidepool_mcp::deliberate_decl()], false);
    preamble.push_str("type ActorEffects = '[Deliberate]\n");
    let source = include_str!("resident_deliberate/parent_program.hs");
    let root = tempfile::tempdir().expect("session root");
    let compile_preamble = insert_preamble_imports(&preamble, "Data.Tree");
    let templates = resident_workbench_templates(&compile_preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
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
    .expect("compile public deliberate program")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("deliberate program should compile as an expression, got {other:?}"),
    };

    let lib = SessionLib::open(session_id, root.path(), ModuleEnv::standalone_default())
        .expect("open declaration plane")
        .with_validation_include(include.clone());
    let machine = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        NoHandlers,
        TestSink,
        include.clone(),
        DEFAULT_NURSERY_SIZE,
        Some(lib),
    )
    .expect("bootstrap actor machine");
    let machines = Arc::new(ActorMachineRegistry::new());
    assert!(machines.insert_idle(session_id, machine).is_none());

    let registry = ActorRegistry::new();
    let starting = registry
        .begin_start(
            None,
            ActorDescriptor::new(
                "resident deliberate",
                ["Deliberate"],
                ActorPlacement {
                    session: session_id,
                    resource_scope: actor_realm,
                    lexical_scope: ScopeId::ROOT,
                },
            ),
            StartInitiator::Runtime,
        )
        .expect("begin actor");
    let actor = registry.publish_ready(starting).expect("publish actor");
    let haskell_turn = registry
        .begin_turn(actor, ActorTurnKind::Haskell)
        .expect("admit authored Haskell turn");

    let checkout = machines
        .checkout_wait(
            session_id,
            CheckoutRequest::Run,
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("checkout actor program");
    let (mut machine, receipt) = checkout.into_parts();
    let context = haskell_turn.session_context();
    machine
        .set_actor_execution(
            context.run_context(),
            context.effect_policy,
            context.live_payload,
        )
        .expect("install actor execution context");
    let initial = machine
        .run_with_sites(
            "resident_deliberate",
            &compiled.expr,
            &compiled.table,
            &compiled.asks,
        )
        .expect("run to deliberate suspension");
    let pending = match initial {
        ResidentOutcome::Suspended { hole, request, .. } => {
            ResidentCompletion::capture(&mut machine, hole, &request, &compiled.table, actor_realm)
                .expect("capture typed completion")
        }
        ResidentOutcome::Completed { .. } => panic!("deliberate must suspend"),
    };
    assert_eq!(pending.request().input_type, "Tree Int");
    assert!(pending
        .request()
        .input_modules
        .iter()
        .any(|module| module == "Data.Tree"));
    assert_eq!(pending.request().output_type, "Bool");
    let holes = machine
        .parked_holes()
        .into_iter()
        .map(str::to_string)
        .collect();
    machines.settle_suspended(receipt, machine, holes);

    let agent = ActorAgentSession::attach(registry.clone(), actor).expect("attach agent session");
    let workbench_source = ActorWorkbenchSource::new(preamble, include);
    let provider = CompletesFromMountedInput {
        requests: Mutex::new(Vec::new()),
    };

    let executor = ResidentCompletionExecutor::new(Arc::clone(&machines), workbench_source);
    let (haskell_turn, completed) = executor
        .resolve(&agent, haskell_turn, &provider, pending, None)
        .await
        .expect("resolve resident completion");
    assert_eq!(haskell_turn.kind(), ActorTurnKind::Haskell);
    drop(haskell_turn);

    match completed {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(84));
        }
        ResidentOutcome::Suspended { .. } => panic!("program should complete after one answer"),
    }
    assert_eq!(provider.requests.lock().len(), 1);
}
