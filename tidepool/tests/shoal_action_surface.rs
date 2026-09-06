//! The public Shoal actor surface.

use std::path::PathBuf;

use tidepool_runtime::compile_haskell;
use tidepool_testing::eval_harness;

fn shoal_include_paths() -> Vec<PathBuf> {
    let declarations = [
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_context_decl(),
        tidepool_mcp::agent_control_decl(),
        tidepool_mcp::agent_inspection_decl(),
        tidepool_mcp::agent_launch_decl(),
        tidepool_mcp::forks_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::fs_read_decl(),
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::bound_worktree_decl(),
        tidepool_mcp::worktree_registry_decl(),
        tidepool_mcp::worktree_allocation_decl(),
        tidepool_mcp::worktree_integration_decl(),
    ];
    let effects =
        tidepool_mcp::ensure_effects_module(&declarations).expect("materialize Shoal effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::repo_root().join("haskell").join("actors"));
    include.push(eval_harness::prelude_path());
    include
}

#[test]
fn resident_deliberation_module_is_not_available() {
    eval_harness::require_extract();
    let include = shoal_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let error = compile_haskell(
        include_str!("shoal_action_surface/deliberation_absent.hs"),
        "result",
        &include_refs,
    )
    .expect_err("the removed resident deliberation module must not compile");
    let failure = tidepool_runtime::classify_compile(&error);
    assert_eq!(
        failure.class,
        tidepool_runtime::FailureClass::UserHaskell,
        "an unavailable optional module is a source error: {}",
        failure.message
    );
    let diagnostic = failure.message;
    assert!(
        diagnostic.contains("Tidepool.Deliberation"),
        "missing-module diagnostic should name the unavailable module:\n{diagnostic}"
    );
}

#[test]
fn shoal_exports_persistent_agents_and_hides_turn_lifecycle_operations() {
    eval_harness::require_extract();
    let include = shoal_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();

    compile_haskell(
        include_str!("shoal_action_surface/public_agents.hs"),
        "result",
        &include_refs,
    )
    .expect("the public facade should expose persistent agents and typed replies");

    compile_haskell(
        include_str!("shoal_action_surface/public_agents.hs"),
        "subSecondRequest",
        &include_refs,
    )
    .expect("dimensional sub-second request deadlines should extract");

    let source = concat!(
        "module ShoalForbidden where\n",
        "import qualified Tidepool.Actors.Shoal as Shoal\n",
        "result = Shoal.complete\n",
    );
    let error = compile_haskell(source, "result", &include_refs)
        .expect_err("the default Shoal facade exposed a completion operation");
    let failure = tidepool_runtime::classify_compile(&error);
    assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
    assert!(failure.message.contains("complete"));

    let source = concat!(
        "module ShoalRawDeadline where\n",
        "import qualified Tidepool.Actors.Shoal as Shoal\n",
        "result = Shoal.requestDeadline 600\n",
    );
    let error = compile_haskell(source, "result", &include_refs)
        .expect_err("the default Shoal facade exposed a bare-integer deadline constructor");
    let failure = tidepool_runtime::classify_compile(&error);
    assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
    assert!(failure.message.contains("requestDeadline"));

    compile_haskell(
        include_str!("shoal_action_surface/coding_can_unfold.hs"),
        "result",
        &include_refs,
    )
    .expect("coding actors can scaffold, fork, observe, and integrate their children");

    let error = compile_haskell(
        include_str!("shoal_action_surface/narrow_research_cannot_control.hs"),
        "result",
        &include_refs,
    )
    .expect_err("an explicitly narrowed researcher unexpectedly acquired actor control");
    assert_eq!(
        tidepool_runtime::classify_compile(&error).class,
        tidepool_runtime::FailureClass::UserHaskell
    );
}

#[test]
fn unresolved_request_result_is_a_source_diagnostic_with_annotation_guidance() {
    eval_harness::require_extract();
    let include = shoal_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let source = include_str!("shoal_action_surface/request_type_diagnostic.hs");
    let error = compile_haskell(source, "unresolved", &include_refs).unwrap_err();
    let tidepool_runtime::CompileError::Diagnostics(diagnostics) = error else {
        panic!("unresolved authored result must be a source rejection: {error:?}");
    };
    assert!(
        diagnostics.iter().any(|diag| {
            diag.message.contains("result type is unresolved")
                && diag.message.contains("requestWith @Finding")
        }),
        "{diagnostics:?}"
    );
    for target in ["annotated", "functionResult"] {
        compile_haskell(source, target, &include_refs)
            .unwrap_or_else(|error| panic!("concrete {target} should compile: {error}"));
    }
}
