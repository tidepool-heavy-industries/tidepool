//! `reflect` reaches the host reader tagged with the actor that is executing.
//!
//! The Haskell verb takes only a count, so there is no argument through which
//! one actor could name another's conversation. These cases hold the other
//! half of that claim: the kernel passes the executing actor and the count it
//! asked for, two actors each get their own answer, and a forest with no
//! reader installed reports every context unbound instead of handing out a
//! conversation that belongs to someone else.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tidepool_actor::{
    ActorExitKind, ActorRef, ActorWorkbenchSource, EffectiveRole, Incarnation, ResidentForest,
};
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_model::{ConversationTurn, Role, TurnItem};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ModuleEnv, OutputSink,
    PreparedTurn, ResidentSession, SessionLib, TurnRequest, TurnResult,
};
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
impl tidepool_effect::dispatch::DispatchEffect<TestSink> for NoHandlers {
    fn dispatch(
        &mut self,
        _: &tidepool_bridge::Value,
        _: &tidepool_effect::dispatch::EffectContext<'_, TestSink>,
    ) -> Result<Option<tidepool_effect::Response>, tidepool_effect::error::EffectError> {
        Ok(None)
    }
}

/// The two completed turns every reader answer below carries. Their content is
/// identical for each actor so the authored program can compare it exactly;
/// what differs per actor is the turn identity, which the call log checks.
fn answer(actor: ActorRef) -> Vec<ConversationTurn> {
    let identity = format!("turn-of-{}-{}", actor.id.0, actor.incarnation.0);
    vec![
        ConversationTurn {
            turn: identity.clone(),
            started_at: Some("opened".into()),
            completed_at: Some("closed".into()),
            items: vec![
                TurnItem::Message {
                    role: Role::User,
                    text: "read the brief".into(),
                },
                TurnItem::ToolCall {
                    call: "c1".into(),
                    tool: "haskell".into(),
                    arguments: "briefQuery".into(),
                },
                TurnItem::ToolResult {
                    call: "c1".into(),
                    output: "the brief".into(),
                },
            ],
        },
        ConversationTurn {
            turn: identity,
            started_at: None,
            completed_at: None,
            items: vec![TurnItem::Message {
                role: Role::Assistant,
                text: "acknowledged".into(),
            }],
        },
    ]
}

#[tokio::test]
async fn each_actor_reflects_its_own_conversation_and_an_unbound_one_reflects_none() {
    eval_harness::require_extract();
    let declarations = [tidepool_mcp::reflect_decl()];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = format!(
        "{}\ntype ActorEffects = '[Reflect]\n",
        insert_preamble_imports(
            &tidepool_mcp::build_preamble(&declarations, false),
            "Tidepool.Effects.Core (Reflect(..), ConversationTurn(..), TurnItem(..), \
             ConversationRole(..), ReflectError(..))",
        )
    );
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().unwrap();
    let compile = |text: &str, gen| match run_turn(TurnRequest {
        turn_text: text,
        templates: &templates,
        include: &include_refs,
        session_root: root.path(),
        inject_modules: &[],
        gen,
        verdict: None,
        target: None,
        prepared: PreparedTurn::first_turn(),
    })
    .unwrap()
    {
        TurnResult::Expr { compiled, .. } => Arc::new(compiled),
        other => panic!("expected a compiled program, got {other:?}"),
    };
    let boot = compile("pure (0 :: Int)", 1);
    let own = compile(include_str!("reflect/own_conversation.hs"), 2);
    let none = compile(include_str!("reflect/no_conversation.hs"), 3);

    let session = tidepool_repr::SessionId((u64::from(std::process::id()) << 32) | 211);
    let workbench = ActorWorkbenchSource::new(preamble.clone(), include.clone());
    let start = |boot: &Arc<tidepool_runtime::session::CompiledTurn>| {
        let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(include.clone());
        let mut machine = ResidentSession::bootstrap(
            &boot.expr,
            boot.table.clone(),
            NoHandlers,
            TestSink,
            include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        )
        .unwrap();
        machine.set_effect_execution(
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        );
        machine
    };

    // A reader is installed, so each actor's own conversation is readable.
    let calls: Arc<Mutex<Vec<(ActorRef, usize)>>> = Arc::default();
    let seen = Arc::clone(&calls);
    let (forest, _events) = ResidentForest::new(
        workbench.clone(),
        session,
        start(&boot),
        None,
        Incarnation::FIRST,
    );
    let forest = forest.with_conversation_reader(Arc::new(move |actor, count| {
        seen.lock().push((actor, count));
        Box::pin(async move { Ok(answer(actor)) })
    }));
    let mut reflected = Vec::new();
    for label in ["reader-a", "reader-b"] {
        let (actor, task) = forest
            .new_program_root(label.into(), EffectiveRole::coding(), Arc::clone(&own))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .unwrap()
            .unwrap();
        let terminal = actor.terminal().wait().await;
        assert_eq!(
            terminal.kind,
            ActorExitKind::Completed,
            "{label}: {terminal:?}"
        );
        reflected.push(actor.identity());
    }
    forest.shutdown().await;

    let calls = calls.lock().clone();
    assert_eq!(
        calls,
        reflected
            .iter()
            .map(|actor| (*actor, 2))
            .collect::<Vec<_>>(),
        "each actor's own identity and requested count reach the reader, and no other actor's do",
    );
    assert_ne!(reflected[0], reflected[1]);

    // No reader installed: the operator-proxy case. Every context is unbound,
    // and none of them is handed the other's conversation instead.
    let (bare, _events) =
        ResidentForest::new(workbench, session, start(&boot), None, Incarnation::FIRST);
    let (actor, task) = bare
        .new_program_root("no-reader".into(), EffectiveRole::coding(), none)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Completed);
    bare.shutdown().await;
}
