//! Runtime proof for named actor effect profiles.
//!
//! All fixtures compile against the write-capable row. Executing the write
//! fixture under a ReadOnly principal therefore bypasses the model-facing GHC
//! guard on purpose and proves the Rust interpreter remains authoritative.

use std::path::{Path, PathBuf};

use tidepool_actor::{
    ActorDescriptor, ActorEffectProfile, ActorOperationClass, ActorPlacement, ActorProfileHandler,
    ActorRef, ActorRegistry, StartInitiator,
};
use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
use tidepool_handlers::{FsReadHandler, FsWriteHandler};
use tidepool_mcp::CapturedOutput;
use tidepool_repr::SessionId;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, ResidentOutcome, ResidentSession,
    TurnRequest as HaskellTurnRequest, TurnResult, TurnTemplate,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness;

struct CompiledProbe {
    expr: tidepool_repr::CoreExpr,
    table: tidepool_repr::DataConTable,
}

fn compile_probe(
    source: &str,
    templates: &[TurnTemplate],
    include: &[PathBuf],
    session_root: &Path,
) -> CompiledProbe {
    let include_refs: Vec<_> = include.iter().map(PathBuf::as_path).collect();
    match run_turn(HaskellTurnRequest {
        turn_text: source,
        templates,
        include: &include_refs,
        session_root,
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .expect("compile filesystem probe")
    {
        TurnResult::Expr { compiled, .. } => CompiledProbe {
            expr: compiled.expr,
            table: compiled.table,
        },
        other => panic!("filesystem probe must compile as an expression, got {other:?}"),
    }
}

fn ready_actor(
    registry: &ActorRegistry,
    profile: ActorEffectProfile,
    session: SessionId,
) -> ActorRef {
    let descriptor = ActorDescriptor::new(
        format!("{profile:?} probe"),
        profile.effect_names().iter().copied(),
        ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope: ScopeId::ROOT,
        },
    )
    .with_profile(profile);
    let starting = registry
        .begin_start(None, descriptor, StartInitiator::Runtime)
        .expect("allocate probe actor");
    registry
        .publish_ready(starting)
        .expect("publish probe actor")
}

fn install_actor<H>(
    session: &mut ResidentSession<H, CapturedOutput>,
    registry: &ActorRegistry,
    actor: ActorRef,
) where
    H: tidepool_effect::DispatchEffect<CapturedOutput> + Send,
{
    let context = registry.session_context(actor).expect("actor context");
    session
        .set_actor_execution(
            context.run_context(),
            context.effect_policy,
            context.live_payload,
        )
        .expect("install actor execution");
}

fn assert_true(outcome: ResidentOutcome) {
    match outcome {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(true));
        }
        ResidentOutcome::Suspended { request, .. } => {
            panic!("filesystem probe unexpectedly suspended on {request:?}")
        }
    }
}

#[test]
fn runtime_profiles_gate_filesystem_handlers_by_exact_principal() {
    eval_harness::require_extract();

    let decls = [tidepool_mcp::fs_read_decl(), tidepool_mcp::fs_write_decl()];
    let effects = tidepool_mcp::ensure_effects_module(&decls).expect("materialize effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let mut preamble = tidepool_mcp::build_preamble(&decls, false);
    preamble.push_str("type ActorEffects = '[FsRead, FsWrite]\n");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let compile_root = tempfile::tempdir().expect("compile root");
    let write = compile_probe(
        include_str!("profile_runtime_authorization/write_probe.hs"),
        &templates,
        &include,
        compile_root.path(),
    );
    let read = compile_probe(
        include_str!("profile_runtime_authorization/read_probe.hs"),
        &templates,
        &include,
        compile_root.path(),
    );

    let registry = ActorRegistry::new();
    let runtime_session = SessionId(0xA7_0001);
    let writer = ready_actor(&registry, ActorEffectProfile::ReadWrite, runtime_session);
    let reader = ready_actor(&registry, ActorEffectProfile::ReadOnly, runtime_session);
    let workspace = tempfile::tempdir().expect("filesystem sandbox");
    let handlers = frunk::hlist![
        ActorProfileHandler::new(
            registry.clone(),
            ActorOperationClass::FsRead,
            FsReadHandler::new(workspace.path().to_path_buf()),
        ),
        ActorProfileHandler::new(
            registry.clone(),
            ActorOperationClass::FsWrite,
            FsWriteHandler::new(workspace.path().to_path_buf()),
        )
    ];
    let mut session = ResidentSession::bootstrap(
        &write.expr,
        write.table.clone(),
        handlers,
        CapturedOutput::new(),
        include,
        DEFAULT_NURSERY_SIZE,
        None,
    )
    .expect("bootstrap resident profile machine");

    install_actor(&mut session, &registry, writer);
    assert_true(
        session
            .run("writer_probe", &write.expr, &write.table)
            .expect("ReadWrite actor writes"),
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("profile-probe.txt"))
            .expect("read written probe"),
        "written by actor"
    );

    install_actor(&mut session, &registry, reader);
    assert_true(
        session
            .run("reader_probe", &read.expr, &read.table)
            .expect("ReadOnly actor reads"),
    );

    let denied = session.run("denied_writer_probe", &write.expr, &write.table);
    assert!(denied.is_err(), "ReadOnly write must fail in Rust");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("profile-probe.txt"))
            .expect("read unchanged probe"),
        "written by actor"
    );
}
