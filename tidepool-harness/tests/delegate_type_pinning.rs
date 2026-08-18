//! PRD 21 C5 — compile-level acceptance for the delegation surface's
//! unnameability. Same evidence class as `finalize_type_pinning.rs`: one
//! deterministic `tidepool-extract` call per case, no model in the loop.
//!
//! The row under test is [`tidepool_harness::answerer_decls_with_delegate`]
//! (`Subagent` prepended to the answerer's `[AskUser, Fork, ReadState,
//! Finalize]`), compiled the same way [`tidepool_harness::harness::run_block`]
//! compiles a delegating window's turn: the block is wrapped as
//! `runDelegate $ do <block>` before it reaches the extract compile (see
//! `EngineConfig::delegate_wrap`'s doc for why the harness does this instead
//! of the model).
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use tidepool_harness::engine::{self, template_turn_for, CompiledTurn, EngineConfig};
use tidepool_harness::{answerer_decls_with_delegate, load_harness_source};
use tidepool_runtime::CompileError;

/// `CompileError`'s own `Display` is only a count; assert against GHC's own
/// message text, same as `finalize_type_pinning.rs`'s tests do.
fn full_diag(e: CompileError) -> String {
    match e {
        CompileError::Diagnostics(diags) => diags
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => other.to_string(),
    }
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

/// The delegating branch-node config: `answerer_decls_with_delegate()` +
/// `examples/harness` on the include path (so `HarnessTypes`/`Decision`
/// resolve, mirroring `finalize_type_pinning.rs`'s `answerer_cfg`).
fn delegating_cfg() -> EngineConfig {
    let mut cfg = EngineConfig::from_decls(
        answerer_decls_with_delegate(),
        repo_root().join("haskell/lib"),
        None,
    )
    .expect("delegating answerer engine config")
    .with_delegate_wrap();
    cfg.include.push(repo_root().join("examples/harness"));
    cfg
}

/// Compile one delegating-window turn — the SAME wrap
/// `Harness::run_block` applies when `cfg.delegate_wrap` is set, applied
/// here by hand since this test drives `template_turn_for` directly rather
/// than the full `Harness`/`SelfHarnessDriver` stack (see
/// `delegate_positive_path.rs` for the end-to-end wiring proof).
fn compile_delegating_turn(
    code: &str,
    imports: &str,
    finalize_ty: Option<&str>,
) -> Result<CompiledTurn, CompileError> {
    let cfg = delegating_cfg();
    let row_imports: Vec<String> = if imports.trim().is_empty() {
        Vec::new()
    } else {
        vec![imports.to_string()]
    };
    let target = match cfg.turn_target(finalize_ty.map(|ty| (ty, row_imports.as_slice()))) {
        Ok(t) => t,
        Err(e) => return Err(CompileError::ExtractFailed(e.to_string())),
    };
    let wrapped = format!("runDelegate $ do\n{code}");
    let src = template_turn_for(&cfg.decls, &target.stack, &wrapped, imports, "");
    engine::compile_turn(
        &cfg.extract_bin,
        &src,
        "result",
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    )
}

/// A block that only ever calls `delegate` — the ONE verb the narrow row
/// admits — must compile fine, and against a REAL `Finalize` pin (the shape
/// every branch-node window's turn actually is).
#[test]
fn delegate_call_compiles_against_the_narrow_row() {
    support::require_extract();
    let code = "do { r <- delegate (DelegateBrief { delegateLabel = \"probe\", \
                 delegateInstruction = \"look around\", delegateExpected = \"\" }); \
                 case r of { Left e -> finalize @Decision (Decision { action = \"observe\", \
                 rationale = renderDelegateError e, confidence = Low }); \
                 Right ok -> finalize @Decision (Decision { action = \"observe\", \
                 rationale = delegateSummary ok, confidence = High }) } }";
    let result = compile_delegating_turn(code, "HarnessTypes", Some("Decision"));
    assert!(
        result.is_ok(),
        "a block that only calls `delegate` must compile against the narrow \
         delegating row, got: {:?}",
        result.err().map(full_diag)
    );
}

/// A block that never calls `delegate` at all — an ordinary `finalize` —
/// must ALSO still compile once wrapped in `runDelegate $ do ...`: the
/// wrap must be a semantic no-op for code that doesn't touch delegation.
///
/// No `:: M ()` annotation on the inner expression — the PROMPT-taught
/// shape (`prompts_prescribed_hole_card_shape_compiles_when_pinned` in
/// `finalize_type_pinning.rs`) never carries one, and it cannot: an
/// explicit `:: M ()` FORCES that statement's type before `runDelegate`'s
/// own signature gets a chance to unify the row through it (the annotation
/// asks for the FULL row directly, `Eff (Subagent ': Worktree ': effs)`,
/// which can never equal `runDelegate`'s expected argument type
/// `Eff (Delegate ': effs) ()`) — an ordinary, recoverable GHC type error a
/// model never actually needs to hit, since it is never what the prompt
/// teaches it to write.
#[test]
fn plain_finalize_still_compiles_under_the_wrap() {
    support::require_extract();
    let code = "finalize @Decision (Decision { action = \"observe\", rationale = \"because\", \
                 confidence = High })";
    let result = compile_delegating_turn(code, "HarnessTypes", Some("Decision"));
    assert!(
        result.is_ok(),
        "runDelegate must be a no-op wrapper for a block that never calls \
         delegate, got: {:?}",
        result.err().map(full_diag)
    );
}

/// THE unnameability proof. Each of these spells a raw `Worktree`/`Subagent`
/// verb or constructor directly — every one must be a COMPILE ERROR naming
/// the `Member` constraint, not "not in scope": the names ARE nameable
/// (vocabulary), the ROW is what refuses them.
#[test]
fn raw_worktree_and_subagent_are_unnameable() {
    support::require_extract();
    let cases = [
        (
            "raw Worktree constructor via send",
            "do { _ <- send (WorktreeCreate (WorktreeSpec { specSource = SourceCurrentRepository, \
             specLabel = \"x\", specDirtyPolicy = RequireClean })); \
             finalize @Decision (Decision { action = \"observe\", rationale = \"\", confidence = Low }) }",
        ),
        (
            "raw Subagent constructor via send",
            "do { _ <- send (SubagentSpawnAsync (spawnSpec (WorktreeSpec { specSource = \
             SourceCurrentRepository, specLabel = \"x\", specDirtyPolicy = RequireClean }) \"x\" \"y\") \
             Aeson.Null); \
             finalize @Decision (Decision { action = \"observe\", rationale = \"\", confidence = Low }) }",
        ),
    ];
    for (label, code) in cases {
        let result = compile_delegating_turn(code, "HarnessTypes", Some("Decision"));
        let err = result.err().unwrap_or_else(|| {
            panic!("{label} must NOT compile against the delegating row — it names a verb the narrow row must refuse")
        });
        let msg = full_diag(err);
        assert!(
            msg.contains("Worktree")
                || msg.contains("Subagent")
                || msg.contains("Member")
                || msg.contains("member"),
            "{label} must fail with an error naming the Member constraint/row, got: {msg}"
        );
    }
}

/// The control for the unnameability proof: the SAME `worktreeCreate` call
/// compiles fine against a row that genuinely carries `Worktree` — so the
/// rejection above comes from the ROW, not from a broken reference to
/// `WorktreeSpec`/`WorktreeCreate` or an unrelated compile failure.
#[test]
fn worktree_verb_compiles_when_the_row_actually_carries_worktree() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(
        vec![
            tidepool_mcp::worktree_decl(),
            tidepool_mcp::askuser_decl(),
            tidepool_mcp::finalize_decl(),
        ],
        repo_root().join("haskell/lib"),
        None,
    )
    .expect("a row that actually carries Worktree");
    let target = cfg
        .turn_target(Some(("Bool", &[])))
        .expect("Bool needs no import");
    let code =
        "do { _ <- send (WorktreeCreate (WorktreeSpec { specSource = SourceCurrentRepository, \
                 specLabel = \"x\", specDirtyPolicy = RequireClean })); finalize @Bool True }";
    let src = template_turn_for(&cfg.decls, &target.stack, code, "", "");
    let result = engine::compile_turn(
        &cfg.extract_bin,
        &src,
        "result",
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    );
    assert!(
        result.is_ok(),
        "worktreeCreate must compile fine when Worktree is genuinely in the \
         row — the control proving the delegating row's rejection above is \
         about the ROW, not a broken reference, got: {:?}",
        result.err().map(full_diag)
    );
}

/// Sanity: the harness's own decls (`load_harness_source`) still typecheck
/// against `answerer_decls_with_delegate()` — this is a compile-target
/// smoke check only, not a full companion run.
#[test]
fn recursive_companion_harness_source_loads() {
    let src =
        load_harness_source(&repo_root().join("harness-dogfooding/recursive-companion/Harness.hs"));
    assert!(
        src.is_ok(),
        "the shipped recursive-companion harness must load: {:?}",
        src.err()
    );
}
