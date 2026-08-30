//! Fresh actor compilation sees only exact facades, never the ambient session
//! module that defined them.

use tidepool_actor::{
    ActorDescriptor, ActorPlacement, ActorRegistry, ActorSourceImports, StartInitiator,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_repr::SessionId;
use tidepool_runtime::session::{ModuleEnv, PersistentSession, SessionLib};
use tidepool_testing::eval_harness;

#[test]
fn descriptor_carries_an_exact_facade_into_an_isolated_compile_view() {
    eval_harness::require_extract();
    let stdlib = eval_harness::prelude_path();
    let root = tempfile::tempdir().expect("session root");
    let lib = SessionLib::open(SessionId(81), root.path(), ModuleEnv::standalone_default())
        .expect("open declaration plane")
        .with_validation_include(vec![stdlib]);
    let mut session = PersistentSession::new(Some(lib), 0, Vec::new(), 1 << 20);
    session
        .define_scoped(&["data Public = Public Int\n\
             data Secret = Secret\n\
             reveal (Public n) = n"])
        .expect("define source module");

    let surface = session
        .exact_exports_in(ScopeId::ROOT, &["Public", "reveal"])
        .expect("select exact exports");
    let source_view = session
        .compile_view_in(ScopeId::ROOT)
        .expect("source compile view");
    let facade = surface
        .materialize(&source_view)
        .expect("materialize facade");

    let actor_scope = session.mint_isolated_scope();
    let actor_view = session
        .compile_view_in(actor_scope)
        .expect("isolated compile view");
    assert_eq!(actor_view.library(), None, "ambient Lib.G must not leak");

    let descriptor = ActorDescriptor::all_suspended(
        "fresh reviewer",
        ["Actor"],
        ActorPlacement {
            session: SessionId(81),
            resource_scope: RealmId::fresh(),
            lexical_scope: actor_scope,
        },
    )
    .with_source_imports(ActorSourceImports::from_exact_facades([&facade]));
    let registry = ActorRegistry::new();
    let starting = registry
        .begin_start(None, descriptor, StartInitiator::Runtime)
        .expect("begin actor startup");
    let actor = registry.publish_ready(starting).expect("publish actor");
    let context = registry.session_context(actor).expect("actor context");

    assert_eq!(
        actor_view.turn_imports(context.source_imports.source_imports()),
        facade.module_name(),
        "the exact facade is the actor's sole session-specific source import"
    );
}
