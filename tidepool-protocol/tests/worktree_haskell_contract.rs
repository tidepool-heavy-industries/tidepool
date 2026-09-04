//! Contract-level rendering checks for the generated Worktree effect.
//!
//! `tidepool-protocol/src/effects/worktree.rs` describes the Worktree effect
//! completely and generates both its Haskell declaration text and Rust wire
//! modules. These literal expectations make model-visible API drift explicit.
//!
//! Every expectation below is a LITERAL string transcribed BY HAND out of
//! `worktree_effect_def!` — not read from a golden file and not derived from
//! the schema itself, which would make the test a tautology. `tidepool-protocol`
//! is a zero-dependency std-only leaf and cannot see `tidepool-mcp`, which is
//! exactly why the expectations have to be hardcoded rather than imported.

use tidepool_protocol::effects::worktree::worktree;
use tidepool_protocol::hs::HsType;

/// The schema must validate before any of its renderings can be trusted.
#[test]
fn worktree_schema_is_valid() {
    if let Err(problems) = worktree().validate() {
        panic!("Worktree schema is invalid:\n  {}", problems.join("\n  "));
    }
}

/// Check the model-visible semantic landmarks without duplicating the whole
/// generated module as a second hand-maintained API definition.
#[test]
fn worktree_type_def_texts_have_semantic_landmarks() {
    let declarations = worktree().type_def_texts();
    assert!(declarations.len() >= 20);
    let submission = declarations
        .iter()
        .find(|declaration| declaration.starts_with("data SubmissionObservation ="))
        .expect("SubmissionObservation declaration");
    for field in [
        "observedWorktreeId :: WorktreeId",
        "baseHead :: GitOid",
        "committedPaths :: [Text]",
        "submittedHead :: HeadState",
        "workingState :: WorkingState",
    ] {
        assert!(submission.contains(field), "missing {field}: {submission}");
    }
    assert!(declarations
        .iter()
        .any(|declaration| declaration.starts_with("data WorktreeError =")));
}

/// Constructor signatures preserve the model-facing roles without copying
/// the complete generated module into the test.
#[test]
fn worktree_constructor_signatures_name_merge_roles() {
    let wt = worktree();
    let signatures = wt.constructor_signatures();
    assert!(signatures.iter().any(|signature| signature
        == "WorktreeTryMerge :: MergeRequest -> Worktree (Either WorktreeError MergeOutcome)"));
    assert!(signatures
        .iter()
        .any(|signature| signature.starts_with("WorktreeCreate ::")));
    assert!(signatures
        .iter()
        .any(|signature| signature.starts_with("WorktreeObserveSubmission ::")));
}

/// Generated thin wrappers retain their important public types. Their prose
/// and formatting are deliberately not a second hand-maintained golden.
#[test]
fn worktree_helpers_expose_typed_operations() {
    let wt = worktree();
    let helpers = wt.helper_texts().join("\n");
    for signature in [
        "createWorktree :: forall effs. Member Worktree effs => WorktreeSpec -> Eff effs (Either WorktreeError WorktreeHandle)",
        "tryMerge :: forall effs. Member Worktree effs => MergeRequest -> Eff effs (Either WorktreeError MergeOutcome)",
        "observeSubmission :: forall effs. Member Worktree effs => WorktreeId -> Eff effs (Either WorktreeError SubmissionObservation)",
    ] {
        assert!(helpers.contains(signature), "missing helper signature: {signature}");
    }
    assert!(helpers.contains("send . WorktreeTryMerge"));
}

/// Rich library helpers must remain absent from the protocol schema rather
/// than entering it as raw Haskell strings. They are imported from
/// `Tidepool.Worktree`; generated code owns only thin wrappers and projections.
#[test]
fn rich_library_helpers_stay_out_of_the_protocol_schema() {
    const NOT_REPRESENTABLE: &[&str] = &[
        "fromCurrentRepository",
        "fromRef",
        "fromWorktree",
        "allowDirtySnapshot",
        "worktreeBranch",
        "worktreeHead",
        "renderWorktreeId",
        "renderGitOid",
        "renderBranchName",
        "renderWorktreeError",
    ];
    let wt = worktree();
    for name in NOT_REPRESENTABLE {
        assert!(
            !wt.helpers.iter().any(|h| h.name == *name),
            "{name} is one of the ten non-representable helpers and must NOT \
             appear in the schema's helper list"
        );
    }
}

/// The remaining decl fields: `description_text()`, `extra_imports`,
/// `prompt_card`, `type_params`, `default_row_args`,
/// `helpers_row_polymorphic`.
#[test]
fn worktree_remaining_decl_fields_are_pinned() {
    let wt = worktree();

    assert_eq!(
        wt.description_text(),
        concat!(
            "Managed git worktrees: create an isolated worktree from a clean source ",
            "(or, explicitly, from a dirty-source snapshot), look a retained one back ",
            "up by durable id, and list what exists. Retain-first — there is no ",
            "release or delete verb in v1, deliberately. Git WORKFLOW (rebase, merge, ",
            "cherry-pick, conflict resolution) is absent by design: that work belongs ",
            "to coding agents with their native tools, and Tidepool observes what the ",
            "repository became through `Tidepool.Event`.",
        )
    );

    // The one row that carries the ten relocated helpers back onto the eval
    // surface: they are now DEFINED in `haskell/lib/Tidepool/Worktree.hs`, and
    // a row containing Worktree imports that module. Matches
    // `extra_imports_for!(Worktree)` in `tidepool-mcp/src/effect_defs.rs`
    // exactly — that agreement is what `worktree_decl_matches_the_schema_exactly`
    // proves against the live macro.
    assert_eq!(wt.extra_imports.to_vec(), vec!["import Tidepool.Worktree"]);
    assert!(wt.prompt_card.is_none());
    assert!(wt.type_params.is_empty());
    assert!(wt.default_row_args.is_empty());
    assert!(wt.helpers_row_polymorphic);
}

/// Exec and Journal both have EMPTY `type_defs`, so the new
/// decls-then-instances-then-error-ADT emission order
/// ([`tidepool_protocol::schema::Effect::type_def_texts`]) must be a no-op for
/// them: Exec still renders exactly its one error-ADT entry, and Journal still
/// renders nothing. `generated_files_are_current.rs` already owns their full
/// pinned bytes — this test only guards that the new Worktree-motivated
/// emission order didn't change their shape.
#[test]
fn exec_and_journal_type_def_emission_order_is_unchanged() {
    let exec = tidepool_protocol::effects::exec::exec();
    assert_eq!(exec.type_def_texts().len(), 1);

    let journal = tidepool_protocol::effects::journal::journal();
    assert!(journal.type_def_texts().is_empty());
}

// ---------------------------------------------------------------------------
// `HelperBody::Projection` — the one shape lane 3 added, and its validator.
// ---------------------------------------------------------------------------

/// The result type is DERIVED by walking the field chain through `type_defs`,
/// never declared. `WorktreeHandle.handleReceipt : WorktreeReceipt` and
/// `WorktreeReceipt.treeId : WorktreeId`, so the signature reads
/// `WorktreeHandle -> WorktreeId` without anyone restating it — retyping either
/// field moves the signature with it instead of letting the two disagree.
/// That is the schema's principle — a restated signature is a drift class —
/// applied to the one helper shape that has no verb to derive from.
#[test]
fn worktree_id_projection_derives_its_result_type_from_the_field_chain() {
    let wt = worktree();
    assert_eq!(
        wt.project(
            &HsType::Named("WorktreeHandle"),
            &["handleReceipt", "treeId"]
        ),
        Ok(HsType::Named("WorktreeId"))
    );
    let rendered = wt
        .helper_texts()
        .into_iter()
        .find(|t| t.contains("worktreeId ::"))
        .expect("worktreeId is a schema helper");
    assert!(rendered.contains("worktreeId :: WorktreeHandle -> WorktreeId\n"));
}

/// A field the declaring `TypeDef` does not have is a GENERATION failure, not a
/// GHC error in emitted source. That is the second thing deriving the result
/// buys, and it is why the walk runs inside `validate` too.
#[test]
fn a_projection_naming_an_absent_field_is_a_generation_failure() {
    let wt = worktree();
    let err = wt
        .project(
            &HsType::Named("WorktreeHandle"),
            &["handleReceipt", "noSuchField"],
        )
        .expect_err("noSuchField is not a WorktreeReceipt field");
    assert!(err.contains("no field `noSuchField`"), "got: {err}");

    let err = wt
        .project(&HsType::Named("DirtySummary"), &["staged", "anything"])
        .expect_err("`[Text]` is not a named type to project out of");
    assert!(err.contains("not a named type"), "got: {err}");

    let err = wt
        .project(&HsType::Named("NotDeclared"), &["x"])
        .expect_err("NotDeclared is not a type_defs entry");
    assert!(err.contains("does not declare"), "got: {err}");
}

/// `Helper::ctor` is an `Option` and BOTH directions are enforced: `Some`
/// exactly for the verb-derived bodies, `None` exactly for a projection. An
/// `Option` only one side checks is how a sentinel constructor name comes back
/// in through the side door.
#[test]
fn the_helper_ctor_pairing_is_enforced_in_both_directions() {
    use tidepool_protocol::schema::{Helper, HelperBody};

    let mut eff = worktree();
    eff.helpers = vec![Helper {
        name: "worktreeId",
        ctor: Some("WorktreeLookup"),
        doc: &[],
        substrate: false,
        body: HelperBody::Projection {
            binder: "h",
            arg: HsType::Named("WorktreeHandle"),
            fields: &["handleReceipt", "treeId"],
        },
    }];
    let errs = eff
        .validate()
        .expect_err("a projection may not name a verb");
    assert!(
        errs.iter().any(|e| e.contains("a projection wraps none")),
        "got: {errs:?}"
    );

    let mut eff = worktree();
    eff.helpers = vec![Helper {
        name: "createWorktree",
        ctor: None,
        doc: &[],
        substrate: false,
        body: HelperBody::Pointfree,
    }];
    let errs = eff
        .validate()
        .expect_err("a send-wrapper must name the verb it wraps");
    assert!(
        errs.iter().any(|e| e.contains(
            "names no verb, but only a \
             pure projection, variant-render, or OPAQUE forward may"
        )),
        "got: {errs:?}"
    );
}
