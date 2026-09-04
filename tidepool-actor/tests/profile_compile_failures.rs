//! Named-profile compile-failure fixtures. Keep profile/row rejections in this
//! family so each assertion does not grow a separate extractor invocation.

use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, TurnRequest as HaskellTurnRequest,
};
use tidepool_testing::eval_harness;

#[test]
fn named_profile_compile_failures() {
    eval_harness::require_extract();

    let decls = [
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::fs_read_decl(),
        tidepool_mcp::fs_write_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&decls).expect("materialize effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let mut preamble = tidepool_mcp::build_preamble(&decls, false);
    preamble.push_str("type ActorEffects = '[Actor, FsRead, FsWrite]\n");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().expect("compile-failure root");
    let error = run_turn(HaskellTurnRequest {
        turn_text: include_str!("profile_compile_failures/read_only_fs_write.hs"),
        templates: &templates,
        include: &include_refs,
        session_root: root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .expect_err("a ReadOnly actor definition must not admit FsWrite");
    let failure = tidepool_runtime::classify_compile(&error.error);
    assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
    assert!(
        failure.message.contains("FsWrite"),
        "the row-membership diagnostic must identify FsWrite:\n{}",
        failure.message
    );
}
