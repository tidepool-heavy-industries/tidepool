//! The public Shoal action surface and the low-level action laws it relies on.

use std::path::PathBuf;

use tidepool_runtime::{compile_and_run_pure, compile_haskell};
use tidepool_testing::eval_harness;

fn shoal_include_paths() -> Vec<PathBuf> {
    let declarations = [
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::deliberate_decl(),
        tidepool_mcp::fs_read_decl(),
        tidepool_mcp::worktree_decl(),
    ];
    let effects =
        tidepool_mcp::ensure_effects_module(&declarations).expect("materialize Shoal effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::repo_root().join("haskell").join("actors"));
    include.push(eval_harness::prelude_path());
    include
}

#[test]
fn shoal_exports_one_lift_helper_with_an_abstract_action_type() {
    eval_harness::require_extract();
    let include = shoal_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();

    let accepted = compile_and_run_pure(
        include_str!("shoal_action_surface/public_action.hs"),
        "result",
        &include_refs,
    )
    .expect("the public facade should expose abstract AgentAction and liftAction");
    let value: serde_json::Value = (&accepted).into();
    assert_eq!(value, serde_json::json!(42));

    let constructor_error = compile_haskell(
        include_str!("shoal_action_surface/constructor_leak.hs"),
        "result",
        &include_refs,
    )
    .expect_err("the default Shoal facade must not expose the AgentAction constructor");
    let constructor_failure = tidepool_runtime::classify_compile(&constructor_error);
    assert_eq!(
        constructor_failure.class,
        tidepool_runtime::FailureClass::UserHaskell
    );
    assert!(
        constructor_failure.message.contains("AgentAction"),
        "unexpected constructor rejection: {}",
        constructor_failure.message
    );
    let runner_error = compile_haskell(
        include_str!("shoal_action_surface/runner_leak.hs"),
        "result",
        &include_refs,
    )
    .expect_err("the default Shoal facade must not expose runAgentAction");
    let runner_failure = tidepool_runtime::classify_compile(&runner_error);
    assert_eq!(
        runner_failure.class,
        tidepool_runtime::FailureClass::UserHaskell
    );
    assert!(
        runner_failure.message.contains("runAgentAction"),
        "unexpected runner rejection: {}",
        runner_failure.message
    );
    let completion_error = compile_haskell(
        include_str!("shoal_action_surface/completion_leak.hs"),
        "result",
        &include_refs,
    )
    .expect_err("the default Shoal facade must not expose generic completion substrate");
    let completion_failure = tidepool_runtime::classify_compile(&completion_error);
    assert_eq!(
        completion_failure.class,
        tidepool_runtime::FailureClass::UserHaskell
    );
    assert!(
        completion_failure.message.contains("Complete")
            || completion_failure.message.contains("complete"),
        "unexpected completion-substrate rejection: {}",
        completion_failure.message
    );
}

#[test]
fn lift_action_preserves_results_and_failures_short_circuit() {
    eval_harness::require_extract();
    let include = shoal_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();

    let evaluated = compile_and_run_pure(
        include_str!("shoal_action_surface/action_laws.hs"),
        "result",
        &include_refs,
    )
    .expect("AgentAction laws should compile and evaluate");
    let value: serde_json::Value = (&evaluated).into();
    assert_eq!(value, serde_json::json!(42));
}
