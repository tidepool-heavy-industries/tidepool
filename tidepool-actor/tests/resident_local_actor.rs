//! The real resident MCP policy driven directly by the canonical local actor.

use tidepool_actor::{
    ActorDescriptor, ActorPlacement, ActorWorkbenchSource, LocalResidentDeployment, ResidentForest,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_eval::Value;
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

#[tokio::test]
async fn local_actor_owns_resident_policy_children_and_terminal_reply() {
    resident_cleanup_case(false).await;
}

#[tokio::test]
async fn authored_failed_shutdown_hook_remains_unconfirmed_in_parent_cleanup() {
    resident_cleanup_case(true).await;
}

async fn resident_cleanup_case(fail_hook: bool) {
    eval_harness::require_extract();

    let session = support::process_unique_session(if fail_hook { 178 } else { 177 });
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
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
        turn_text: if fail_hook {
            include_str!("resident_local_actor/policy_failed_hook.hs")
        } else {
            include_str!("resident_local_actor/policy.hs")
        },
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
    let sibling_scope = machine.mint_isolated_scope();
    let sibling_realm = RealmId::fresh();
    machine
        .set_actor_execution(
            tidepool_runtime::session::SessionRunContext {
                resource_scope: sibling_realm,
                lexical_scope: sibling_scope,
                ..tidepool_runtime::session::SessionRunContext::ROOT
            },
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        )
        .expect("independent root scope");
    let sibling_outcome = machine
        .run_with_sites(
            "resident_sibling_policy",
            &compiled.expr,
            &compiled.table,
            &compiled.asks,
        )
        .expect("sibling policy boundary");
    let sibling_descriptor = ActorDescriptor::new(
        "sibling",
        ActorPlacement {
            session,
            resource_scope: sibling_realm,
            lexical_scope: sibling_scope,
        },
    );
    let (forest, mut deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        None,
        tidepool_actor::Incarnation::FIRST,
    );
    for invalid in [
        ActorDescriptor::new(
            "foreign",
            ActorPlacement {
                session: support::process_unique_session(178),
                ..descriptor.placement()
            },
        ),
        descriptor
            .clone()
            .with_supervisor_parent(tidepool_actor::ActorRef {
                id: tidepool_actor::ActorId(1),
                incarnation: tidepool_actor::Incarnation::FIRST,
            }),
        descriptor
            .clone()
            .with_context_parent(tidepool_actor::ActorRef {
                id: tidepool_actor::ActorId(1),
                incarnation: tidepool_actor::Incarnation::FIRST,
            }),
    ] {
        assert!(forest
            .admit_root(
                invalid,
                tidepool_runtime::session::ResidentOutcome::BindingsCommitted {
                    output: Vec::new()
                },
            )
            .await
            .is_err());
    }
    let (actor, task) = forest
        .admit_root(descriptor, outcome)
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
    let (sibling, sibling_task) = forest
        .admit_root(sibling_descriptor, sibling_outcome)
        .await
        .expect("admit independent sibling");
    let LocalResidentDeployment::PolicyInstalled(sibling_installation) = deployments
        .recv()
        .await
        .expect("sibling policy installation")
    else {
        panic!("sibling retired before installation")
    };
    assert_eq!(sibling_installation.actor.identity(), sibling.identity());
    assert_eq!(sibling_installation.supervisor_parent, None);
    assert_eq!(sibling_installation.context_parent, None);
    let sibling_server =
        tidepool_mcp::DynamicMcpServer::from_resident_policy(sibling_installation.policy)
            .expect("sibling MCP projection");
    let changed = sibling_server
        .dispatch_tool(
            "set_value",
            serde_json::json!({"next": 73}).as_object().unwrap().clone(),
        )
        .await
        .expect("change sibling state");
    assert_eq!(
        changed.structured_content,
        Some(serde_json::json!({"current": 73}))
    );

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
        Some(serde_json::json!({"started": !fail_hook}))
    );
    let child_retired = deployments.recv().await.expect("child retirement");
    assert!(matches!(
        child_retired,
        LocalResidentDeployment::Retired { ref terminal, .. }
            if terminal.kind == if fail_hook { tidepool_actor::ActorExitKind::Failed } else { tidepool_actor::ActorExitKind::Completed }
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
    let cleanup = actor.terminal().cleanup().expect("retained cleanup");
    assert_eq!(cleanup.actor(), actor.identity());
    assert_eq!(cleanup.is_confirmed(), !fail_hook, "{cleanup:?}");
    assert!(matches!(
        cleanup.realm(),
        tidepool_actor::CleanupComponentOutcome::Confirmed
    ));
    if fail_hook {
        assert!(matches!(
            cleanup.children(),
            tidepool_actor::CleanupComponentOutcome::Unconfirmed(_)
        ));
    }

    assert_eq!(
        actor.terminal().wait().await.kind,
        tidepool_actor::ActorExitKind::Completed
    );
    assert!(matches!(
        deployments.recv().await,
        Some(LocalResidentDeployment::Retired { actor: retired, .. })
            if retired == actor.identity()
    ));
    // The first tree's retirement must preserve the sibling's live closures.
    let retained = sibling_server
        .dispatch_tool("current_value", serde_json::Map::new())
        .await
        .expect("sibling survives root retirement");
    assert_eq!(
        retained.structured_content,
        Some(serde_json::json!({"current": 73}))
    );
    sibling_server
        .dispatch_tool("finish_value", serde_json::Map::new())
        .await
        .expect("retire sibling");
    sibling_task.await.expect("sibling task");
    assert_eq!(
        sibling.terminal().wait().await.kind,
        tidepool_actor::ActorExitKind::Completed
    );
}
