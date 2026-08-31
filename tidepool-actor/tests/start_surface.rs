//! Haskell construction proof for actor startup, stopping at the Rust
//! orchestration boundary. A public `ActorSpec` becomes one live rooted child
//! entry; that entry performs typed startup deliberation, reaches the private
//! readiness effect, and then completes its installed program.

use tidepool_actor::ResidentActorStart;
use tidepool_bridge::ToCore;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_eval::Value;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, OutputSink, ResidentOutcome, ResidentSession,
    TurnRequest, TurnResult,
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

#[test]
fn public_spec_crosses_as_one_rooted_entry_and_reaches_readiness() {
    eval_harness::require_extract();

    let decls = [
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::deliberate_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&decls).expect("materialize effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let mut preamble = tidepool_mcp::build_preamble(&decls, false);
    preamble.push_str("type AgentEffects = '[Actor]\n");
    let templates = resident_workbench_templates(&preamble, "AgentEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().expect("session root");
    let source = r#"
let workerSpec :: ActorSpec Int Maybe Int
    workerSpec =
      actorSpec
        "worker"
        (\seed ->
          deliberate
            (deliberation @Int @Bool "Approve the supplied seed.")
            seed)
        (\seed approved ->
          (pure (if approved then seed + 1 else seed - 1)
            :: Eff '[Deliberate, ActorLocal Maybe Int] Int))
in do
    _ <- startActor workerSpec 41
    pure (7 :: Int)
"#;
    let compiled = match run_turn(TurnRequest {
        turn_text: source,
        templates: &templates,
        include: &include_refs,
        session_root: root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .expect("compile public actor start")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("actor start should compile as an expression, got {other:?}"),
    };

    let mut session = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        NoHandlers,
        vec!["Actor".into(), "ActorLocal".into(), "Deliberate".into()],
        TestSink,
        include,
        DEFAULT_NURSERY_SIZE,
        None,
    )
    .expect("bootstrap resident machine");
    session.set_effect_execution(
        EffectRunPolicy::SuspendAll,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );

    let child_realm = RealmId::fresh();
    let parent = session
        .run("start_parent", &compiled.expr, &compiled.table)
        .expect("run parent to start suspension");
    let start = match parent {
        ResidentOutcome::Suspended { hole, request, .. } => ResidentActorStart::capture(
            &mut session,
            hole,
            &request,
            &compiled.table,
            compiled.asks.clone(),
            child_realm,
        )
        .expect("capture rooted actor entry"),
        ResidentOutcome::Completed { .. } => panic!("startActor must suspend"),
    };
    assert_eq!(start.request().label, "worker");
    let (parent_hole, entry, _sites) = start.into_parts();

    let startup = session
        .run_rooted_entry("actor_entry", entry, 0, child_realm, Some(&compiled.table))
        .expect("run child entry");
    let (startup_hole, startup_request) = match startup {
        ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        ResidentOutcome::Completed { .. } => panic!("prompted startup must deliberate"),
    };
    assert_eq!(
        tidepool_effect::dispatch::request_constructor(&startup_request, &compiled.table)
            .rsplit('.')
            .next(),
        Some("DeliberateWith")
    );

    let ready = session
        .resume(
            startup_hole,
            true.to_value(&compiled.table).expect("box startup answer"),
        )
        .expect("resume startup deliberation");
    let ready_hole = match ready {
        ResidentOutcome::Suspended { hole, request, .. } => {
            assert_eq!(
                tidepool_effect::dispatch::request_constructor(&request, &compiled.table)
                    .rsplit('.')
                    .next(),
                Some("ActorReadyWith")
            );
            assert_eq!(session.parked_realm(&hole), Some(child_realm));
            assert!(session.live_payload_handle(hole.cont_id()).is_none());
            hole
        }
        ResidentOutcome::Completed { .. } => panic!("entry must park at readiness"),
    };

    let child_done = session
        .resume(
            ready_hole,
            ().to_value(&compiled.table).expect("box readiness unit"),
        )
        .expect("run installed one-shot program");
    assert!(matches!(child_done, ResidentOutcome::Completed { .. }));

    let parent_done = session
        .resume(
            parent_hole,
            (1_i64, 1_i64)
                .to_value(&compiled.table)
                .expect("box actor identity"),
        )
        .expect("publish exact actor identity to parent");
    match parent_done {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(7));
        }
        ResidentOutcome::Suspended { .. } => panic!("parent should complete after start"),
    }
}
