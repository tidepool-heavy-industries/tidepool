//! Public one-way facade compilation, including opaque receipt construction.
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, TurnRequest, TurnResult,
};
use tidepool_testing::eval_harness;

#[test]
fn notification_facade_compiles_and_receipt_constructor_is_private() {
    eval_harness::require_extract();
    let declarations = [tidepool_mcp::notifications_decl()];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    include.push(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../haskell/actors"));
    let mut preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        "qualified Tidepool.Actors.Shoal as Shoal",
    );
    preamble.push_str("type ActorEffects = '[Shoal.Notifications]\n");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().unwrap();
    let compile = |text, gen| {
        run_turn(TurnRequest {
            turn_text: text,
            templates: &templates,
            include: &include_refs,
            session_root: root.path(),
            inject_modules: &[],
            gen,
            verdict: None,
            target: None,
        })
    };
    assert!(matches!(
        compile(include_str!("notifications/facade.hs"), 1).unwrap(),
        TurnResult::Expr { .. }
    ));
    let rejected = compile(
        "pure (Shoal.NotificationReceipt ((1,1), ((2,1), (\"inbox\", 1))))",
        2,
    )
    .expect_err("receipt constructor must not be publicly exported");
    let failure = tidepool_runtime::classify_compile(&rejected.error);
    assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
    assert!(
        failure.message.contains("NotificationReceipt"),
        "{}",
        failure.message
    );
}

#[derive(Clone, Default)]
struct TestSink;
impl tidepool_runtime::session::OutputSink for TestSink {
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
        _: &tidepool_eval::Value,
        _: &tidepool_effect::dispatch::EffectContext<'_, TestSink>,
    ) -> Result<Option<tidepool_effect::Response>, tidepool_effect::error::EffectError> {
        Ok(None)
    }
}

/// Precompile an authored program with Notifications, then admit it under an
/// attenuated runtime role. Effect membership is deliberately not authority.
#[tokio::test]
async fn notification_interpreter_denies_role_and_stale_target_then_continues() {
    use std::{sync::Arc, time::Duration};
    use tidepool_actor::{
        ActorEffectKey, ActorExitKind, ActorWorkbenchSource, EffectiveRole, Incarnation,
        LocalResidentDeployment, ResidentForest,
    };
    use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
    use tidepool_runtime::session::{ModuleEnv, ResidentSession, SessionLib};

    eval_harness::require_extract();
    let declarations = [tidepool_mcp::notifications_decl()];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = format!(
        "{}\ntype ActorEffects = '[Notifications]\n",
        insert_preamble_imports(
            &tidepool_mcp::build_preamble(&declarations, false),
            "Tidepool.Effects.Core (Notifications(..))"
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
    })
    .unwrap()
    {
        TurnResult::Expr { compiled, .. } => Arc::new(compiled),
        _ => panic!("expected compiled program"),
    };
    let boot = compile("pure (0 :: Int)", 1);
    let session = tidepool_repr::SessionId((u64::from(std::process::id()) << 32) | 198);
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
    let (forest, mut events) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble.clone(), include.clone()),
        session,
        machine,
        None,
        Incarnation::FIRST,
    );
    let peer = forest
        .new_workbench("notification-peer".into(), EffectiveRole::coding())
        .await
        .unwrap();
    let target = peer.identity();
    let address = format!("({}, {})", target.id.0, target.incarnation.0);
    let stale = format!("({}, {})", target.id.0, target.incarnation.0 + 1);

    // All standard profiles permit Notifications; the existing host attenuation
    // API supplies the honest denial context without adding production policy.
    let denied_role = EffectiveRole::coding().with_effect_keys(vec![ActorEffectKey::Replies]);
    assert!(denied_role.respects_role_ceiling());
    for (gen, denied) in [(2, true), (3, false)] {
        let program = format!(
            "({}) {} {} {}",
            include_str!("notifications/rejection.hs"),
            address,
            if denied { &address } else { &stale },
            if denied { "True" } else { "False" }
        );
        let admission = forest.new_program_root(
            format!("rejection-{gen}"),
            if denied {
                denied_role.clone()
            } else {
                EffectiveRole::coding()
            },
            compile(&program, gen),
        );
        // Root startup drives its program before admission returns. The host
        // must service handoffs concurrently rather than await that admission.
        let host = async {
            if denied {
                return None;
            }
            loop {
                if let LocalResidentDeployment::NotificationSend(command) =
                    events.recv().await.unwrap()
                {
                    assert_eq!(command.target(), target);
                    assert_eq!(command.message(), "continued after stale rejection");
                    let owner = command.owner();
                    command.rejected(tidepool_actor::NotificationError::Unavailable);
                    return Some(owner);
                }
            }
        };
        let (admission, observed_owner) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(admission, host)
        })
        .await
        .expect("notification rejection and continuation must finish");
        let (actor, task) = admission.unwrap();
        assert_eq!(
            observed_owner,
            if denied { None } else { Some(actor.identity()) }
        );
        tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .unwrap()
            .unwrap();
        let terminal = actor.terminal().wait().await;
        assert_eq!(terminal.kind, ActorExitKind::Completed, "{terminal:?}");
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event, LocalResidentDeployment::NotificationSend(_)),
                "denied or stale send must never reach host"
            );
        }
    }
    forest.shutdown().await;
    assert_eq!(peer.terminal().wait().await.kind, ActorExitKind::Cancelled);
}
