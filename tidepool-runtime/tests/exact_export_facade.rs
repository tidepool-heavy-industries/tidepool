//! Exact actor export membrane over the real GHC/session declaration path.

use tidepool_codegen::scope::ScopeId;
use tidepool_repr::SessionId;
use tidepool_runtime::compile_and_run_pure_salted;
use tidepool_runtime::session::{ModuleEnv, PersistentSession, SessionLib};
use tidepool_testing::eval_harness;

#[test]
fn facade_preserves_exact_types_without_leaking_ambient_declarations() {
    eval_harness::require_extract();
    let stdlib = eval_harness::prelude_path();
    let root = tempfile::tempdir().expect("session root");
    let lib = SessionLib::open(SessionId(71), root.path(), ModuleEnv::standalone_default())
        .expect("open declaration plane")
        .with_validation_include(vec![stdlib.clone()]);
    let mut session = PersistentSession::new(Some(lib), 0, Vec::new(), 1 << 20);
    session
        .lib_mut()
        .define(
            "data Public = Public Int\n\
             data Secret = Secret\n\
             reveal (Public n) = n",
        )
        .expect("define source module");

    let surface = session
        .exact_exports_in(ScopeId::ROOT, &["Public", "reveal"])
        .expect("select exact exports");
    let view = session
        .compile_view_in(ScopeId::ROOT)
        .expect("compile view");
    let facade = surface.materialize(&view).expect("materialize facade");

    let accepted = format!(
        "module Probe where\n\
         import {}\n\
         result :: Int\n\
         result = reveal (Public 42)\n",
        facade.module_name()
    );
    let include = [root.path(), stdlib.as_path()];
    let result = compile_and_run_pure_salted(
        &accepted,
        "result",
        &include,
        Some("exact-export-facade-accepted"),
    )
    .expect("selected exact exports compile and run");
    let json: serde_json::Value = (&result).into();
    assert_eq!(json, serde_json::json!(42));

    let rejected = format!(
        "module Probe where\n\
         import {}\n\
         result :: Int\n\
         result = case Secret of {{ Secret -> 0 }}\n",
        facade.module_name()
    );
    assert!(
        compile_and_run_pure_salted(
            &rejected,
            "result",
            &include,
            Some("exact-export-facade-rejected"),
        )
        .is_err(),
        "an ambient declaration omitted from the membrane must not be nameable"
    );
}
