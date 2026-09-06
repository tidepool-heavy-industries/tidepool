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
