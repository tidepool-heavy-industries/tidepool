//! Real GHC → Core → JIT proof that an actor lifecycle lease, actor-local
//! scopes, and an exact execution principal mount together on the existing
//! resident-session substrate.

use tidepool_actor::{
    mount_actor_turn, ActorDescriptor, ActorPlacement, ActorRegistry, ActorTurnKind, StartInitiator,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_runtime::session::{OutputSink, ResidentOutcome, ResidentSession};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness::{self, mock, EvalHarness};

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

struct AsSink<Handlers>(Handlers);

impl<Handlers: DispatchEffect<()>> DispatchEffect<TestSink> for AsSink<Handlers> {
    fn dispatch(
        &mut self,
        request: &Value,
        context: &EffectContext<'_, TestSink>,
    ) -> Result<Option<Response>, EffectError> {
        let unit_context = EffectContext::with_user(context.table(), &());
        self.0.dispatch(request, &unit_context)
    }
}

#[test]
fn ready_actor_runs_haskell_under_its_exact_principal() {
    eval_harness::require_extract();
    let compiler = EvalHarness::new().with_stdlib();
    let source = mock::mcp_module("result :: M Int\nresult = pure (42 :: Int)");
    let compiled = compiler
        .compile(&source, "result")
        .expect("compile actor turn");

    let mut session = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        AsSink(mock::min_stack()),
        TestSink,
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
        None,
    )
    .expect("bootstrap resident session");
    let lexical_scope = session
        .mint_scope(ScopeId::ROOT)
        .expect("mint actor lexical scope");
    let registry = ActorRegistry::new();
    let starting = registry
        .begin_start(
            None,
            ActorDescriptor::new(
                "literal actor",
                mock::EFFECT_NAMES.iter().copied(),
                ActorPlacement {
                    session: tidepool_repr::SessionId(1),
                    resource_scope: RealmId::fresh(),
                    lexical_scope,
                },
            ),
            StartInitiator::Runtime,
        )
        .expect("begin actor startup");
    let actor = registry.publish_ready(starting).expect("publish actor");
    let context = registry.session_context(actor).expect("actor context");
    let lease = mount_actor_turn(&registry, &mut session, actor, ActorTurnKind::Haskell)
        .expect("mount actor turn");

    assert_eq!(session.run_context(), context.run_context());
    assert_eq!(session.effect_policy(), context.effect_policy);
    let outcome = session
        .run("actor_literal", &compiled.expr, &compiled.table)
        .expect("run actor turn");
    match outcome {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(42));
        }
        ResidentOutcome::Suspended { .. } => panic!("pure actor turn unexpectedly suspended"),
    }
    drop(lease);

    registry
        .begin_turn(actor, ActorTurnKind::AgentSession)
        .expect("actor admission restored after resident execution");
}

#[test]
fn actor_mount_installs_its_request_policy_and_effect_metadata() {
    eval_harness::require_extract();
    let compiler = EvalHarness::new().with_stdlib();
    let source =
        mock::mcp_module("result :: M ()\nresult = send (Print \"actor-local interpreter\")");
    let compiled = compiler
        .compile(&source, "result")
        .expect("compile actor effect turn");
    let effect_names: Vec<String> = mock::EFFECT_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let mut session = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        AsSink(mock::min_stack()),
        TestSink,
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
        None,
    )
    .expect("bootstrap resident session");
    let lexical_scope = session
        .mint_scope(ScopeId::ROOT)
        .expect("mint actor lexical scope");
    let registry = ActorRegistry::new();
    let starting = registry
        .begin_start(
            None,
            ActorDescriptor::new(
                "effect actor",
                effect_names.clone(),
                ActorPlacement {
                    session: tidepool_repr::SessionId(1),
                    resource_scope: RealmId::fresh(),
                    lexical_scope,
                },
            ),
            StartInitiator::Runtime,
        )
        .expect("begin actor startup");
    let actor = registry.publish_ready(starting).expect("publish actor");
    let descriptor = registry.descriptor(actor).expect("actor descriptor");
    assert_eq!(descriptor.effect_names(), effect_names);
    assert_eq!(
        descriptor.effect_policy(),
        tidepool_effect::EffectRunPolicy::SuspendAll
    );

    let _lease = mount_actor_turn(&registry, &mut session, actor, ActorTurnKind::Haskell)
        .expect("mount actor turn");
    assert_eq!(session.effect_policy(), descriptor.effect_policy());
    assert_eq!(
        session.live_payload_policy(),
        descriptor.live_payload_policy()
    );
    assert!(
        matches!(
            session
                .run("actor_effect", &compiled.expr, &compiled.table)
                .expect("run actor effect"),
            ResidentOutcome::Suspended { .. }
        ),
        "Console is below the session's original Ask threshold, but an all-suspended actor must route it to its actor-local interpreter"
    );
}
