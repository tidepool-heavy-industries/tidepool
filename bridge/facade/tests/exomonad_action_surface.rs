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
        tidepool_mcp::resource_scopes_decl(),
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
    include.push(PathBuf::from(
        std::env::var_os("TIDEPOOL_HASKELL_ACTORS_DIR")
            .expect("TIDEPOOL_HASKELL_ACTORS_DIR must name the declared actor source resource"),
    ));
    include.push(eval_harness::prelude_path());
    include
}

#[test]
fn parameterized_invocation_row_keeps_external_nominal_owner_imports() {
    use tidepool_testing::effect_surface::{TestEffectSurface, TestEffectSurfaceOptions};

    eval_harness::require_extract();
    let protocol = tempfile::tempdir().unwrap();
    std::fs::write(
        protocol.path().join("ExternalRowProtocol.hs"),
        include_str!("exomonad_action_surface/ExternalRowProtocol.hs"),
    )
    .unwrap();
    let declarations = [tidepool_mcp::actor_local_decl()];
    let surface = TestEffectSurface::with_options(
        &declarations,
        TestEffectSurfaceOptions {
            row_args: tidepool_mcp::RowArgs::at("ActorLocal", ["ExternalRowProtocol.Protocol"])
                .importing(["qualified ExternalRowProtocol"]),
            ..Default::default()
        },
    )
    .unwrap();
    let mut includes = surface.include_path_refs();
    includes.push(protocol.path());
    let source = tidepool_runtime::session::assemble_opaque_expression_module(
        surface.preamble(),
        "result",
        surface.row(),
        "pure (41 :: Int)",
        tidepool_runtime::session::ExpressionLift::Effectful,
    );
    tidepool_testing::with_settlement(|settlement| {
        compile_haskell(&source, "result", &includes, settlement)
    })
    .expect("the explicit invocation row resolves its external nominal protocol owner");
}

#[test]
fn reusable_event_and_async_helpers_compile_with_narrow_rows() {
    eval_harness::require_extract();
    let effects = tidepool_mcp::ensure_effects_module(&[
        tidepool_mcp::event_decl(),
        tidepool_mcp::green_decl(),
    ])
    .expect("materialize reusable helper vocabulary");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();

    tidepool_testing::with_settlement(|settlement| {
        compile_haskell(
            include_str!("exomonad_action_surface/reusable_helper_rows.hs"),
            "result",
            &refs,
            settlement,
        )
    })
    .expect("one checked source supports Green-only, RepoEvent-only and empty rows");

    let error = tidepool_testing::with_settlement(|settlement| {
        compile_haskell(
            include_str!("exomonad_action_surface/async_without_green.hs"),
            "result",
            &refs,
            settlement,
        )
    })
    .expect_err("calling async requires Green even when its module imports successfully");
    assert_eq!(
        tidepool_runtime::classify_compile(&error).class,
        tidepool_runtime::FailureClass::UserHaskell
    );
}

#[test]
fn public_agents_accept_raw_inputs_and_dimensional_deadlines() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let source = include_str!("exomonad_action_surface/public_agents.hs");
    for target in [
        "result",
        "submitRaw",
        "submitWithProgress",
        "submitWithOnlyReplies",
        "subSecondRequest",
    ] {
        tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, target, &include_refs, settlement)
        })
        .unwrap_or_else(|error| panic!("public {target} should compile: {error}"));
    }

    let raw_deadline = concat!(
        "{-# LANGUAGE DataKinds, TypeApplications #-}\n",
        "module RawDeadline where\n",
        "import qualified Tidepool.Actors.Exomonad as Exomonad\n",
        "result agent = Exomonad.request @Bool agent (7 :: Int) ",
        "(Exomonad.defaultRequestOptions { Exomonad.requestDeadline = Just (600 :: Int) })\n",
    );
    let error = tidepool_testing::with_settlement(|settlement| {
        compile_haskell(raw_deadline, "result", &include_refs, settlement)
    })
    .expect_err("request deadlines require Duration rather than an integer");
    let failure = tidepool_runtime::classify_compile(&error);
    assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
    assert!(failure.message.contains("Duration"), "{}", failure.message);

    tidepool_testing::with_settlement(|settlement| {
        compile_haskell(
            include_str!("exomonad_action_surface/skill_promised_names.hs"),
            "result",
            &include_refs,
            settlement,
        )
    })
    .expect("the public GitOid renderer remains callable");
}

#[test]
fn independent_spawns_select_context_and_workspace() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    for (source, targets) in [
        (
            include_str!("exomonad_action_surface/minimal_spawn.hs"),
            &["spawnMinimal"][..],
        ),
        (
            include_str!("exomonad_action_surface/independent_children.hs"),
            &["spawnPair", "askBoth"][..],
        ),
        (
            include_str!("exomonad_action_surface/spawn_context_workspace_choices.hs"),
            &[
                "freshFork",
                "forkFromCheckpoint",
                "sharedDirectory",
                "retainedWorkspace",
            ][..],
        ),
    ] {
        for target in targets {
            tidepool_testing::with_settlement(|settlement| {
                compile_haskell(source, target, &include_refs, settlement)
            })
            .unwrap_or_else(|error| panic!("explicit {target} should compile: {error}"));
        }
    }
}

#[test]
fn narrow_rows_refuse_unavailable_spawn_and_control_effects_after_valid_controls() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    tidepool_testing::with_settlement(|settlement| {
        compile_haskell(
            include_str!("exomonad_action_surface/minimal_spawn.hs"),
            "spawnMinimal",
            &include_refs,
            settlement,
        )
    })
    .expect("a row with AgentLaunch admits the explicit spawn expression");
    tidepool_testing::with_settlement(|settlement| {
        compile_haskell(
            include_str!("exomonad_action_surface/public_agents.hs"),
            "stop",
            &include_refs,
            settlement,
        )
    })
    .expect("a row with AgentControl accepts ordinary retirement");

    for (source, target, missing_effect) in [
        (
            include_str!("exomonad_action_surface/cannot_spawn_without_agent_launch.hs"),
            "branchOnSpawnAuthority",
            "AgentLaunch",
        ),
        (
            include_str!("exomonad_action_surface/helper_needs_spawn_effect.hs"),
            "spawnHelper",
            "AgentLaunch",
        ),
        (
            include_str!("exomonad_action_surface/cannot_control_without_agent_control.hs"),
            "stopNeedsControl",
            "AgentControl",
        ),
    ] {
        let error = tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, target, &include_refs, settlement)
        })
        .expect_err("importing a helper does not grant its required effect");
        let failure = tidepool_runtime::classify_compile(&error);
        assert_eq!(failure.class, tidepool_runtime::FailureClass::UserHaskell);
        assert!(
            failure.message.contains(missing_effect),
            "{}",
            failure.message
        );
    }
}

#[test]
fn authored_delegation_composes_separate_spawn_request_and_observation_admissions() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let source = include_str!("exomonad_action_surface/delegate_request.hs");
    for target in ["delegateAndAwait", "delegateAndWatch"] {
        tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, target, &include_refs, settlement)
        })
        .unwrap_or_else(|error| panic!("authored {target} should compile: {error}"));
    }
}

#[test]
fn unresolved_request_result_is_a_source_diagnostic_with_annotation_guidance() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let source = include_str!("exomonad_action_surface/request_type_diagnostic.hs");
    let error = tidepool_testing::with_settlement(|settlement| {
        compile_haskell(source, "unresolved", &include_refs, settlement)
    })
    .unwrap_err();
    let tidepool_runtime::CompileError::Diagnostics(diagnostics) = error else {
        panic!("unresolved authored result must be a source rejection: {error:?}");
    };
    assert!(
        diagnostics.iter().any(|diag| {
            diag.message.contains("result type is unresolved")
                && diag.message.contains("request @Finding")
        }),
        "{diagnostics:?}"
    );
    for target in ["annotated", "functionAnswer"] {
        tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, target, &include_refs, settlement)
        })
        .unwrap_or_else(|error| panic!("concrete {target} should compile: {error}"));
    }
}

#[test]
fn ordinary_text_labels_work_for_spawn_request_and_watch() {
    eval_harness::require_extract();
    let include = exomonad_include_paths();
    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let source = include_str!("exomonad_action_surface/ordinary_labels.hs");
    for target in [
        "spawnWithOrdinaryLabel",
        "requestWithOrdinaryLabel",
        "watchWithOrdinaryLabel",
    ] {
        tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, target, &include_refs, settlement)
        })
        .unwrap_or_else(|error| panic!("ordinary Text label {target} should compile: {error}"));
    }
}
