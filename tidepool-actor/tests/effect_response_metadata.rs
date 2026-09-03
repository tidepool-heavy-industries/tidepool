//! Effect responses cross from Rust back into Haskell. Their constructors
//! must remain present even when the model-facing module exports the result
//! type abstractly and the submitted expression never constructs it itself.

use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn,
    TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_testing::eval_harness;

#[test]
fn abstract_effect_response_constructors_are_in_the_turn_table() {
    eval_harness::require_extract();

    let declarations = [
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::worktree_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("worktree effect");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let mut preamble = tidepool_mcp::build_preamble(&declarations, false);
    preamble.push_str("type ActorEffects = '[AgentSession, Worktree]\n");
    let preamble = insert_preamble_imports(&preamble, "Tidepool.Deliberation");
    let templates = resident_workbench_templates(
        &preamble,
        "(Complete (AgentAction ActorEffects (Maybe ActionFailure)) ': ActorEffects)",
        "Tidepool.Agent.Action",
    );
    let include_refs = include
        .iter()
        .map(std::path::PathBuf::as_path)
        .collect::<Vec<_>>();
    let session_root = tempfile::tempdir().expect("session root");

    let compiled = match run_turn(HaskellTurnRequest {
        turn_text: concat!(
            "complete $ nextTurn $ AgentAction $ do\n",
            "  tree <- createWorktree (fromCurrentRepository \"metadata-probe\")\n",
            "  pure (Right tree)"
        ),
        templates: &templates,
        include: &include_refs,
        session_root: session_root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .expect("compile worktree effect expression")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("worktree request should be an expression, got {other:?}"),
    };

    assert!(
        compiled
            .table
            .get_by_name_arity("WorktreeHandle", 1)
            .is_some(),
        "the Rust interpreter cannot return WorktreeHandle without its hidden constructor metadata"
    );
}
