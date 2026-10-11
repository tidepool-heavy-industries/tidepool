//! Exact-row compile-failure fixtures. Keep profile/row rejections in this
//! family so each assertion does not grow a separate extractor invocation.

use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn,
    TurnRequest as HaskellTurnRequest,
};
use tidepool_testing::effect_surface::{TestEffectSurface, TestEffectSurfaceOptions};
use tidepool_testing::eval_harness;

#[test]
fn selected_effect_row_compile_failures() {
    eval_harness::require_extract();

    let decls = [
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::fs_read_decl(),
        tidepool_mcp::fs_write_decl(),
    ];
    let effects = TestEffectSurface::with_options(
        &decls,
        TestEffectSurfaceOptions {
            companion_imports: tidepool_mcp::CompanionImports::Include,
            user_library: false,
            ..Default::default()
        },
    )
    .expect("materialize effects");
    let include = effects.include_paths();
    let mut preamble =
        insert_preamble_imports(effects.preamble(), "Tidepool.Effects.Row (knownEffects)");
    preamble.push_str("type ActorEffects = '[Actor, FsRead, FsWrite]\n");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().expect("compile-failure root");
    tidepool_testing::with_settlement(|settlement| {
        run_turn(
            HaskellTurnRequest {
                exact_context: None,
                session_id: None,
                turn_text: include_str!("profile_compile_failures/selected_valid.hs"),
                templates: &templates,
                include: &include_refs,
                session_root: root.path(),
                inject_modules: &[],
                gen: 1,
                verdict: None,
                target: None,
                retained_imports: &[],
            },
            settlement,
        )
    })
    .expect("the same empty selected row admits a behavior with no external effects");
    let error = tidepool_testing::with_settlement(|settlement| {
        run_turn(
            HaskellTurnRequest {
                exact_context: None,
                session_id: None,
                turn_text: include_str!("profile_compile_failures/selected_fs_write.hs"),
                templates: &templates,
                include: &include_refs,
                session_root: root.path(),
                inject_modules: &[],
                gen: 2,
                verdict: None,
                target: None,
                retained_imports: &[],
            },
            settlement,
        )
    })
    .expect_err("an actor definition with no selected effects must not admit FsWrite");
    let failure = tidepool_runtime::classify_compile(&error.error);
    assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
    assert!(
        failure.message.contains("FsWrite"),
        "the row-membership diagnostic must identify FsWrite:\n{}",
        failure.message
    );
}
