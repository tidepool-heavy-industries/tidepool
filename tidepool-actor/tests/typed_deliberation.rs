//! End-to-end typed deliberation over the resident GHC/JIT workbench.
//!
//! The provider emits ordinary fenced Haskell. A declaration committed by the
//! first block is used by the second block to complete with an effectful
//! closure. The closure is retained in the actor realm, the disposable block
//! realm is closed, and the value remains callable on a later checkout.

use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_actor::{
    run_result_session, ActorAgentSession, ActorDescriptor, ActorMachineRegistry, ActorPlacement,
    ActorRegistry, ActorWorkbenchSource, CompletionExpectation, ResidentActorWorkbench,
    StartInitiator,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::Value;
use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage};
use tidepool_repr::SessionId;
use tidepool_runtime::session::registry::{CheckoutRequest, SlotKind};
use tidepool_runtime::session::{
    ModuleEnv, OutputSink, ResidentOutcome, ResidentSession, SessionLib,
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

type Machine = ResidentSession<NoHandlers, TestSink>;

struct WorkbenchProvider {
    machines: Arc<ActorMachineRegistry<NoHandlers, TestSink>>,
    session: SessionId,
    requests: Mutex<Vec<TurnRequest>>,
}

impl ModelProvider for WorkbenchProvider {
    async fn complete(
        &self,
        request: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        assert_ne!(
            self.machines.kind(self.session),
            Some(SlotKind::Running),
            "provider inference must never hold the resident machine checkout"
        );
        let mut requests = self.requests.lock();
        let round = requests.len();
        if round > 1 {
            return Err(ProviderError::Auth("test script exhausted".into()));
        }
        if round == 1 {
            let diagnostic = request
                .messages
                .last()
                .map(|message| message.content.as_str())
                .unwrap_or("<missing diagnostic>");
            assert!(
                diagnostic.contains("block 3/4 rejected")
                    && diagnostic.contains("suffix did not run"),
                "unexpected corrective diagnostic:\n{diagnostic}"
            );
        }
        requests.push(request);
        drop(requests);
        Ok(TurnResponse {
            text: if round == 0 {
                concat!(
                    "```haskell\n",
                    "twice f x = f (f x)\n",
                    "```\n",
                    "```haskell\n",
                    "offset <- pure (twice (+ 1) 38)\n",
                    "```\n",
                    "```haskell\n",
                    "complete \"wrong type\"\n",
                    "```\n",
                    "```haskell\n",
                    "error \"rejected completion must stop the suffix\"\n",
                    "```"
                )
                .into()
            } else {
                concat!(
                    "```haskell\n",
                    "complete ((\\n -> pure (offset + twice (+ 1) n)) :: Int -> Eff ActorEffects Int)\n",
                    "```\n",
                    "```haskell\n",
                    "error \"completion must stop the suffix\"\n",
                    "```"
                )
                .into()
            },
            usage: Usage::default(),
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

#[tokio::test]
async fn fenced_haskell_returns_a_live_typed_closure_without_holding_checkout() {
    eval_harness::require_extract();

    let session_id = SessionId(91);
    let actor_realm = RealmId::fresh();
    let root = tempfile::tempdir().expect("session root");
    let effects = tidepool_mcp::ensure_effects_module(&[tidepool_mcp::actor_decl()])
        .expect("materialize actor effects module");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());

    let lib = SessionLib::open(session_id, root.path(), ModuleEnv::standalone_default())
        .expect("open declaration plane")
        .with_validation_include(include.clone());
    let machine: Machine = ResidentSession::unbootstrapped(
        NoHandlers,
        TestSink,
        include.clone(),
        DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let machines = Arc::new(ActorMachineRegistry::new());
    assert!(machines.insert_idle(session_id, machine).is_none());

    let registry = ActorRegistry::new();
    let starting = registry
        .begin_start(
            None,
            ActorDescriptor::new(
                "typed deliberation",
                ["Actor"],
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
    let agent = ActorAgentSession::attach(registry, actor).expect("attach agent session");
    let mut admitted = agent.begin_agent_session().expect("admit agent session");

    let mut preamble = tidepool_mcp::build_preamble(&[tidepool_mcp::actor_decl()], false);
    preamble.push_str("type ActorEffects = '[Actor]\n");
    let source = ActorWorkbenchSource::new(preamble, include);
    let expected = "Int -> Eff ActorEffects Int";
    let mut workbench =
        ResidentActorWorkbench::new(Arc::clone(&machines), source, expected, Vec::new());
    let provider = WorkbenchProvider {
        machines: Arc::clone(&machines),
        session: session_id,
        requests: Mutex::new(Vec::new()),
    };

    let entry = run_result_session(
        &mut admitted,
        &provider,
        &mut workbench,
        CompletionExpectation::new(
            "Build an effectful increment-by-two entry function.",
            expected,
        ),
        None,
        None,
    )
    .await
    .expect("typed completion");
    drop(admitted);

    assert_eq!(provider.requests.lock().len(), 2);
    let checkout = machines
        .checkout_wait(
            session_id,
            CheckoutRequest::Run,
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("checkout retained closure");
    let (mut machine, receipt) = checkout.into_parts();
    let outcome = machine
        .run_rooted_entry("retained_deliberation", entry, 0, actor_realm, None)
        .expect("invoke retained closure");
    let holes = machine
        .parked_holes()
        .into_iter()
        .map(str::to_string)
        .collect();
    machines.settle_suspended(receipt, machine, holes);

    match outcome {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(42));
        }
        ResidentOutcome::Suspended { .. } => panic!("pure retained closure must complete"),
    }
}
