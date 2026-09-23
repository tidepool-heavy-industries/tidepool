//! The public Exomonad actor surface.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
use std::path::PathBuf;

use tidepool_runtime::compile_haskell;
use tidepool_testing::eval_harness;

fn exomonad_include_paths() -> Vec<PathBuf> {
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
        tidepool_mcp::lookup_decl(),
    ];
    let effects =
        tidepool_mcp::ensure_effects_module(&declarations).expect("materialize Exomonad effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::repo_root().join("bridge/haskell/actors"));
    include.push(eval_harness::prelude_path());
    include
}

#[test]
fn node_mailboxes_compile_with_opaque_ids() {
    eval_harness::require_extract();
    let effects = tidepool_mcp::ensure_effects_module(&[
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::event_decl(),
        tidepool_mcp::green_decl(),
    ])
    .expect("materialize Node effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    compile_haskell(
        include_str!("exomonad_action_surface/node_mailbox_surface.hs"),
        "result",
        &refs,
    )
    .expect("Node mailboxes use the generated opaque MailboxId surface");
}

#[test]
fn resident_deliberation_module_is_not_available() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let error = compile_haskell(
        include_str!("exomonad_action_surface/deliberation_absent.hs"),
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
fn exomonad_exports_persistent_agents_and_hides_turn_lifecycle_operations() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();

    compile_haskell(
        include_str!("exomonad_action_surface/public_agents.hs"),
        "result",
        &include_refs,
    )
    .expect("the public facade should expose persistent agents and typed replies");

    compile_haskell(
        include_str!("exomonad_action_surface/public_agents.hs"),
        "subSecondRequest",
        &include_refs,
    )
    .expect("dimensional sub-second request deadlines should extract");

    let source = concat!(
        "module ExomonadForbidden where\n",
        "import qualified Tidepool.Actors.Exomonad as Exomonad\n",
        "result = Exomonad.complete\n",
    );
    let error = compile_haskell(source, "result", &include_refs)
        .expect_err("the default Exomonad facade exposed a completion operation");
    let failure = tidepool_runtime::classify_compile(&error);
    assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
    assert!(failure.message.contains("complete"));

    let source = concat!(
        "module ExomonadRawDeadline where\n",
        "import qualified Tidepool.Actors.Exomonad as Exomonad\n",
        "result = Exomonad.requestDeadline 600\n",
    );
    let error = compile_haskell(source, "result", &include_refs)
        .expect_err("the default Exomonad facade exposed a bare-integer deadline constructor");
    let failure = tidepool_runtime::classify_compile(&error);
    assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
    assert!(failure.message.contains("requestDeadline"));

    for hidden in ["reply", "pollReply", "actorContext"] {
        let source = format!(
            "module ExomonadHidden where\nimport qualified Tidepool.Actors.Exomonad as Exomonad\nresult = Exomonad.{hidden}\n"
        );
        let error = compile_haskell(&source, "result", &include_refs)
            .expect_err("the default Exomonad facade exposed a hidden operation");
        let failure = tidepool_runtime::classify_compile(&error);
        assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
        assert!(failure.message.contains(hidden), "{}", failure.message);
    }

    compile_haskell(
        include_str!("exomonad_action_surface/skill_promised_names.hs"),
        "result",
        &include_refs,
    )
    .expect("a name a shipped skill uses in a worked example must be callable from a cell");

    compile_haskell(
        include_str!("exomonad_action_surface/coding_can_unfold.hs"),
        "result",
        &include_refs,
    )
    .expect("coding actors can scaffold, fork, observe, and integrate their children");

    compile_haskell(
        include_str!("exomonad_action_surface/research_can_unfold.hs"),
        "result",
        &include_refs,
    )
    .expect("research coordinators can fork researchers and explicit leaves");

    let error = compile_haskell(
        include_str!("exomonad_action_surface/research_leaf_cannot_unfold.hs"),
        "result",
        &include_refs,
    )
    .expect_err("research leaves must not acquire Forks");
    assert_eq!(
        tidepool_runtime::classify_compile(&error).class,
        tidepool_runtime::FailureClass::UserHaskell
    );

    let error = compile_haskell(
        include_str!("exomonad_action_surface/narrow_research_cannot_control.hs"),
        "result",
        &include_refs,
    )
    .expect_err("an explicitly narrowed researcher unexpectedly acquired actor control");
    assert_eq!(
        tidepool_runtime::classify_compile(&error).class,
        tidepool_runtime::FailureClass::UserHaskell
    );
}

/// A read-only errand is one call: the cell names a task and reads a reply,
/// with no record, client, state query or retirement of its own. Its typed
/// site lives in `Tidepool.Actors.Unfold` rather than in the cell, which is
/// why the reply is `Text` and not a caller-chosen result type. Only a row
/// carrying `AgentLaunch` may issue one, so the inspection-only child an
/// errand starts cannot start another.
#[test]
fn an_errand_is_one_call_and_only_a_launch_row_may_issue_one() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let source = include_str!("exomonad_action_surface/errand.hs");

    for target in ["askText", "reply", "typedStillWorks"] {
        compile_haskell(source, target, &include_refs).unwrap_or_else(|error| {
            panic!("a read-only errand should be one callable statement ({target}): {error}")
        });
    }

    let denied = include_str!("exomonad_action_surface/errand_needs_launch.hs");
    for target in ["result", "fromResearch"] {
        let error = compile_haskell(denied, target, &include_refs)
            .expect_err("a row without AgentLaunch must not be able to issue an errand");
        let failure = tidepool_runtime::classify_compile(&error);
        assert_eq!(
            failure.class,
            tidepool_runtime::FailureClass::UserHaskell,
            "{}",
            failure.message
        );
    }
}

#[test]
fn unresolved_request_result_is_a_source_diagnostic_with_annotation_guidance() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let source = include_str!("exomonad_action_surface/request_type_diagnostic.hs");
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
