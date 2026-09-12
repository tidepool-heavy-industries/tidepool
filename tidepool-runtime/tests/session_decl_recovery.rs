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
    first.define("removed = 7 :: Int").unwrap();
    first
        .define_with_vals(
            "residentDependent = stable + 1",
            &["Tidepool.Data.Text".into()],
            &[],
        )
        .unwrap();
    first
        .define("downstream = residentDependent + 1 :: Int")
        .unwrap();
    first.define("independent = stable + 2 :: Int").unwrap();
    first.seed_scope(ScopeId(7), first.scope_tip(ScopeId::ROOT));
    first
        .define_scoped_in(ScopeId(7), "childOnly = 99 :: Int")
        .unwrap();
    first.retract("residentDependent").unwrap();
    first.retract("removed").unwrap();

    let manifest_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    let manifest_text = manifest_json.to_string();
    assert!(!manifest_text.contains("childOnly"));
    for forbidden in ["Tidepool.Data.Text", "handles", "grants", "effects"] {
        assert!(
            !manifest_text.contains(forbidden),
            "source manifest retained resident authority: {forbidden}"
        );
    }

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
    assert_eq!(report.replayed.len(), 3);
    assert_eq!(report.lost.len(), 2);
    assert!(report
        .lost
        .iter()
        .any(|lost| lost.reason.contains("depended on resident values")));
    assert!(report.lost.iter().any(|lost| {
        lost.sources
            .iter()
            .any(|source| source.contains("downstream"))
            && lost.reason.contains("residentDependent")
    }));
    assert!(report
        .replayed
        .windows(2)
        .all(|pair| pair[0].source_generation < pair[1].source_generation
            && pair[0].successor_generation < pair[1].successor_generation));
    assert!(report
        .replayed
        .iter()
        .all(|replayed| replayed.origin_session == 70));
    assert_eq!(run_probe(&lib_dir, &successor, "stable + independent"), 82);
    assert_eq!(
        successor.current_decl_heads(),
        vec![("independent".into(), 3), ("stable".into(), 1)],
        "retracted, child-scoped, resident-dependent and downstream declarations must stay absent"
    );
}

#[test]
fn successor_replays_shadowed_nominal_types_with_their_original_functions() {
    let lib_dir = setup();
    let cache_root = tempfile::tempdir().unwrap();
    // SAFETY: this integration test owns its process and sets the cache root
    // before compiling either incarnation.
    unsafe { std::env::set_var("XDG_CACHE_HOME", cache_root.path()) };

    let durable = tempfile::tempdir().unwrap();
    let manifest = durable.path().join("nominal-declarations.json");
    let first_root = tempfile::tempdir().unwrap();
    let mut first = SessionLib::open(
        SessionId(72),
        first_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(vec![lib_dir.clone()]);
    first.attach_recovery_manifest(&manifest).unwrap();

    let old_type = first
        .define("data Version = Old Int")
        .expect("define original Version");
    let old_reader = first
        .define("readOld :: Version -> Int\nreadOld (Old value) = value")
        .expect("define original Version reader");
    let new_type = first
        .define("data Version = New T.Text")
        .expect("shadow Version with a distinct nominal type");
    let new_reader = first
        .define("readNew :: Version -> Int\nreadNew (New _) = 4")
        .expect("define shadowed Version reader");
    assert_eq!(
        (old_type, old_reader, new_type, new_reader),
        (Generation(1), Generation(2), Generation(3), Generation(4),)
    );

    let successor_root = tempfile::tempdir().unwrap();
    let mut successor = SessionLib::open(
        SessionId(73),
        successor_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(vec![lib_dir.clone()]);
    let report = successor.attach_recovery_manifest(&manifest).unwrap();
    assert_eq!(report.replayed.len(), 4, "{report:?}");
    assert!(report.lost.is_empty(), "{report:?}");

    let source = format!(
        "module Probe where\n\
         import qualified Data.Text as T\n\
         import qualified Tidepool.Session.Lib.G{old_reader} as OldGeneration\n\
         import qualified Tidepool.Session.Lib.G{new_reader} as NewGeneration\n\
         result :: Int\n\
         result = OldGeneration.readOld (OldGeneration.Old 3) + NewGeneration.readNew (NewGeneration.New (T.pack \"fresh\"))\n"
    );
    let result = compile_and_run_pure_salted(
        &source,
        "result",
        &[successor.include_dir(), lib_dir.as_path()],
        Some(&successor.cache_salt()),
    )
    .unwrap_or_else(|error| {
        panic!("recovered nominal probe failed:\n{source}\n--- error ---\n{error}")
    });
    let json: serde_json::Value = (&result).into();
    assert_eq!(json, serde_json::json!(7));
}
