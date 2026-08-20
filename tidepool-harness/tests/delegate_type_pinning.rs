//! PRD 21 C5 — compile-level acceptance for the delegation surface's
//! unnameability, AND (the reshape) that a delegating window's model text
//! compiles at exactly the row `type M` names. Same evidence class as
//! `finalize_type_pinning.rs`: one deterministic `tidepool-extract` call per
//! case, no model in the loop.
//!
//! The row under test is [`tidepool_harness::answerer_decls_with_delegate`]
//! (`Subagent`/`Worktree` prepended to the answerer's `[AskUser, Fork,
//! ReadState, Finalize]`). `runDelegate` is applied at the compiled turn's
//! RESULT position now (`engine::template_turn_for`'s `delegate_wrap`
//! argument), never as a text prepend to the model's block — see
//! `EngineConfig::delegate_wrap`'s doc for the full mechanism and why the
//! harness does this instead of the model.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::sync::Arc;

use tidepool_harness::engine::{self, template_turn_for, CompiledTurn, EngineConfig};
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{answerer_decls_with_delegate, load_harness_source, Harness, TurnOutcome};
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

/// Compile one delegating-window turn against the row `type M` names (PRD 21
/// C5's reshape) — `runDelegate` is applied by the EXPR template's own
/// RESULT position (`EngineConfig::delegate_wrap`'s doc, `engine::template_turn_for`),
/// never by a text prepend to `code`: this test hands `code` straight
/// through, unmodified, exactly as `Harness::run_block` now does. Driven
/// directly rather than through the full `Harness`/`SelfHarnessDriver` stack
/// (see `delegate_positive_path.rs` for the end-to-end wiring proof).
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
    let src = template_turn_for(
        &cfg.decls,
        &target.stack,
        code,
        imports,
        "",
        cfg.delegate_wrap,
    );
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

/// A block ANNOTATED `:: M ()` at its terminal statement — the shape a model
/// fluent in canonical GHCi idiom reaches for on its own — must compile
/// against the row `type M` NAMES for this delegating window, i.e. the narrow
/// `[Delegate, AskUser, Fork, ReadState, Finalize T]`, not the outer
/// `[Subagent, Worktree, ...]` row the machine actually dispatches. Before
/// the reshape this was the live PRD 21 C5 defect (companion dogfood,
/// 2026-08-20): `M` was the OUTER row, so the annotation forced a type
/// `runDelegate` could never unify its argument against, surfacing as a
/// baffling doubled-`Worktree` GHC diagnostic rather than the model's actual
/// mistake (if any).
#[test]
fn annotated_m_type_compiles_against_the_narrow_row() {
    support::require_extract();
    let code = "do { r <- (delegate (DelegateBrief { delegateLabel = \"probe\", \
                 delegateInstruction = \"look around\", delegateExpected = \"\" }) :: M (Either DelegateError DelegateResult)); \
                 case r of { Left e -> finalize @Decision (Decision { action = \"observe\", \
                 rationale = renderDelegateError e, confidence = Low }); \
                 Right ok -> finalize @Decision (Decision { action = \"observe\", \
                 rationale = delegateSummary ok, confidence = High }) } }";
    let result = compile_delegating_turn(code, "HarnessTypes", Some("Decision"));
    assert!(
        result.is_ok(),
        "a statement annotated `:: M (...)` must compile against the row `type \
         M` names for this delegating window, got: {:?}",
        result.err().map(full_diag)
    );
}

/// A block that never calls `delegate` at all — an ordinary `finalize` —
/// must ALSO still compile once wrapped by `runDelegate`: the wrap must be a
/// semantic no-op for code that doesn't touch delegation.
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
    let src = template_turn_for(&cfg.decls, &target.stack, code, "", "", false);
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

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "delegate-type-pinning".into(),
        extract_fingerprint: "delegate-type-pinning".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 50,
        output_tokens: 10,
        cached_input_tokens: None,
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: content.to_string(),
        usage: usage(),
    }
}

/// PRD 21 C5 acceptance (b): a top-level `data` DECLARATION compiles and
/// defines successfully in a delegating window, and is usable in a LATER
/// turn on the same node — the shape `run_block`'s OLD `runDelegate $ do
/// <block>` text prepend made impossible (it reached the `Decl` candidate
/// too, turning a bare `data` declaration into invalid Haskell). Single-item
/// blocks (one item per turn), not the multi-item block lane — this is the
/// case the reshape's `run_block` fix targets directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_declaration_defines_and_is_usable_in_a_delegating_window() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("delegate_decl_persists.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = delegating_cfg();
    let replies = vec![
        // Turn 1: a bare top-level `data` declaration — a single-item block,
        // classified as `Decl` and committed to the node's decl plane.
        reply("```haskell\ndata Marker = Marker Text\n```"),
        // Turn 2: uses the type turn 1 defined. Only resolves if the decl
        // genuinely persisted, unmangled, into the session.
        reply(
            "```haskell\npure (toJSON (case Marker \"delegating-window-marker\" of \
             { Marker t -> t }))\n```",
        ),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "delegate decl-persistence root",
            "Declare a type, then use it, in a delegating window.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let turn1 = harness
        .run_to_hole_or_done(root)
        .await
        .expect("turn 1 (the data declaration) must compile and define, unwrapped");
    assert!(
        matches!(turn1, TurnOutcome::Completed { .. }),
        "the declaration turn should complete, got {}",
        outcome_tag(&turn1)
    );

    let turn2 = harness
        .follow_up(root, "Now use the type you declared.")
        .await
        .expect("turn 2 must compile against the SAME delegating row and see `Marker`");
    match &turn2 {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains("delegating-window-marker"),
                "the value must come from turn 1's decl surviving into turn 2, \
                 got: {rendered}"
            );
        }
        other => panic!("expected turn 2 to complete, got {}", outcome_tag(other)),
    }
}

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
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
