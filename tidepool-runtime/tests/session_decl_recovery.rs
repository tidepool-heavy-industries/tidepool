//! Source-only recovery for a lost resident declaration machine.

use std::path::{Path, PathBuf};

use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{Generation, SessionId};
use tidepool_runtime::session::{ModuleEnv, SessionLib};
use tidepool_runtime::{compile_and_run_pure_salted, paths};
use tidepool_testing::eval_harness;

fn setup() -> PathBuf {
    eval_harness::require_extract();
    let lib = eval_harness::prelude_path();
    assert!(lib.exists());
    lib
}

fn probe(gen: Generation, expr: &str) -> String {
    format!(
        "module Probe where\n\
         import Tidepool.Session.Lib.G{gen}\n\
         result :: Int\n\
         result = {expr}\n"
    )
}

fn run_probe(lib_dir: &Path, session: &SessionLib, expr: &str) -> serde_json::Value {
    let source = probe(session.generation(), expr);
    let result = compile_and_run_pure_salted(
        &source,
        "result",
        &[session.include_dir(), lib_dir],
        Some(&session.cache_salt()),
    )
    .unwrap_or_else(|error| panic!("probe failed:\n{source}\n--- error ---\n{error}"));
    (&result).into()
}

#[test]
fn successor_replays_only_root_source_independent_of_resident_values() {
    let lib_dir = setup();
    let cache_root = tempfile::tempdir().unwrap();
    // SAFETY: this integration test is its own process and sets the cache root
    // before starting compilation or threads.
    unsafe { std::env::set_var("XDG_CACHE_HOME", cache_root.path()) };
    assert!(paths::cache_dir().starts_with(cache_root.path()));

    let durable = tempfile::tempdir().unwrap();
    let manifest = durable.path().join("root-declarations.json");
    let first_root = tempfile::tempdir().unwrap();
    let mut first = SessionLib::open(
        SessionId(70),
        first_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(vec![lib_dir.clone()]);
    let initial = first.attach_recovery_manifest(&manifest).unwrap();
    assert!(initial.replayed.is_empty());

    first.define("stable = 40 :: Int").unwrap();
    first
        .define_with_vals(
            "residentDependent = stable + 1",
            &["Tidepool.Data.Text".into()],
            &[],
        )
        .unwrap();
    first.seed_scope(ScopeId(7), first.scope_tip(ScopeId::ROOT));
    first
        .define_scoped_in(ScopeId(7), "childOnly = 99 :: Int")
        .unwrap();
    first.retract("residentDependent").unwrap();

    let successor_root = tempfile::tempdir().unwrap();
    let mut successor = SessionLib::open(
        SessionId(71),
        successor_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(vec![lib_dir.clone()]);
    let report = successor.attach_recovery_manifest(&manifest).unwrap();

    assert_eq!(report.source_session, Some(70));
    assert_eq!(report.successor_session, 71);
    assert_eq!(report.replayed.len(), 1);
    assert_eq!(report.lost.len(), 1);
    assert!(report.lost[0]
        .reason
        .contains("depended on resident values"));
    assert_eq!(run_probe(&lib_dir, &successor, "stable + 2"), 42);
    assert_eq!(
        successor.current_decl_heads(),
        vec![("stable".into(), 1)],
        "child scopes and resident-dependent declarations must not enter root recovery"
    );
}
