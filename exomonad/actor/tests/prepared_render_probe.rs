//! F0 probe (temporary; folded into `tidepool/runtime/tests/prepared_turn.rs`
//! at F3): does the production cell-render module project under prepared STG?
//!
//! The real cell render (`render_cell_observation`) is a BIND of a
//! `DisplayPage`-typed statement through the shared workbench templates, so
//! the probe exercises exactly that shape, plus a dialect-sensitive
//! expression (defaulting, `OverloadedStrings`) and the opaque fallback.
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, TurnRequest, TurnResult,
};
use tidepool_testing::eval_harness;

fn probe(label: &str, text: &str, gen: u64) {
    eval_harness::require_extract();
    let declarations = [tidepool_mcp::notifications_decl()];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    include.push(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/actors"),
    );
    let mut preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        "qualified Tidepool.Actors.Exomonad as Exomonad",
    );
    preamble.push_str("type ActorEffects = '[Exomonad.Notifications]\n");
    let templates = resident_workbench_templates(
        &preamble,
        "ActorEffects",
        "qualified Tidepool.Inspection as TidepoolInspection\nTidepool.Inspection (print, cellDisplay)",
    );
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().unwrap();
    let result = run_turn(TurnRequest {
                session_id: None,
        turn_text: text,
        templates: &templates,
        include: &include_refs,
        session_root: root.path(),
        inject_modules: &[],
        gen,
        verdict: None,
        target: None,
        retained_imports: &[],
    })
    .unwrap_or_else(|error| panic!("{label}: turn failed: {error}"));
    let compiled = match &result {
        TurnResult::Bind { compiled, .. } | TurnResult::Expr { compiled, .. } => compiled,
        TurnResult::Decl { .. } => panic!("{label}: classified as a declaration"),
    };
    let prepared = &compiled.prepared;
    // The DataConTable and prepared closure share constructor identities: wherever the table and the
    // prepared closure both declare a constructor, the prepared `host_id` IS
    // the host id (both are minted by Tidepool.Identity.varId). The prepared
    // closure legitimately declares more (base's Typeable/exception
    // machinery recovered through fat interfaces), so it is not a subset.
    let mut shared = 0usize;
    let disagreeing: Vec<_> = prepared
        .constructors()
        .iter()
        .filter_map(|declaration| {
            let qualified = format!(
                "{}.{}",
                declaration.identity.module, declaration.identity.occurrence
            );
            let core_id = compiled.table.get_by_qualified_name(&qualified)?;
            shared += 1;
            (core_id != declaration.host_id).then_some((qualified, core_id, declaration.host_id))
        })
        .collect();
    assert!(
        disagreeing.is_empty(),
        "{label}: constructors whose Core id and prepared host_id differ: {disagreeing:?}"
    );
    assert!(
        shared > 0,
        "{label}: no constructor is shared with the Core table"
    );
    eprintln!(
        "{label}: projected ({} constructors, {} shared with the Core table, {} globals)",
        prepared.constructors().len(),
        shared,
        prepared.globals().len()
    );
}

#[test]
fn display_page_render_bind_projects() {
    probe(
        "render bind",
        "__tidepoolPage1 <- pure ((TidepoolInspection.displayPageWithout [] 8192 \
         ((\\() -> (Right (Just (3 :: Int)) :: Either Text (Maybe Int))) ())) \
         :: TidepoolInspection.DisplayPage ActorEffects)",
        1,
    );
}

#[test]
fn dialect_sensitive_expression_projects() {
    probe(
        "dialect expr",
        "length (show (2 ^ 10)) + T.length \"abc\"",
        2,
    );
}

#[test]
fn opaque_render_fallback_projects() {
    probe(
        "opaque bind",
        "__tidepoolPage2 <- pure ((TidepoolInspection.pageWithContinuation 8192 \
         (TidepoolInspection.TextLeaf (T.pack \"<opaque value>\")) Nothing) \
         :: TidepoolInspection.DisplayPage ActorEffects)",
        3,
    );
}
