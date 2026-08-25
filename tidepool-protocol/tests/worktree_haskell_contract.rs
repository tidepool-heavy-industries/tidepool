//! The pre-flip BYTE proof for the Worktree effect.
//!
//! `tidepool-protocol/src/effects/worktree.rs` describes the Worktree effect
//! completely, but it is deliberately NOT in [`tidepool_protocol::effects::all`]
//! — flipping it would write generated modules into `tidepool-mcp` and
//! `tidepool-handlers` whose mod-index collides with the still-live
//! `worktree_effect_def!` macro in `tidepool-mcp/src/effect_defs.rs`. This test
//! proves the schema renders IDENTICAL Haskell to that macro before the flip
//! happens, the same way `generated_files_are_current.rs`'s
//! `exec_contract_text_is_pinned` / `journal_contract_text_is_pinned` pin Exec
//! and Journal.
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

/// The 20 `type_defs` literals from the macro (13 `data` decls, then 7
/// `instance ToJSON` lines), in exactly the macro's order, plus a 21st entry:
/// the `WorktreeError` ADT as `error_decl_text!` / `error_variant_text!` /
/// `error_variant_json_arm!` expand it for the macro's 11 variants, plus a
/// 14th `data` decl added AFTER the flip: `MergeOutcome`, the typed result of
/// the `WorktreeMergeInto` verb (no `ToJSON` instance — `json: JsonInstance::
/// None`, same as `WorktreeReceipt`/`WorktreeHandle`/`WorktreeSummary` — so it
/// sits among the `data` decls, before the `ToJSON` block, not at the end).
#[test]
fn worktree_type_def_texts_are_pinned() {
    let wt = worktree();

    assert_eq!(
        wt.type_def_texts(),
        vec![
            // -- 13 `data` decls, macro order --------------------------------
            "data WorktreeId = WorktreeId Text deriving (Show, Eq)",
            "data GitOid = GitOid Text deriving (Show, Eq)",
            "data GitRef = GitRef Text deriving (Show, Eq)",
            "data BranchName = BranchName Text deriving (Show, Eq)",
            "data WorktreeSource = SourceCurrentRepository | SourceRef GitRef | SourceWorktree WorktreeId deriving (Show, Eq)",
            "data DirtyPolicy = RequireClean | AllowDirtySnapshot deriving (Show, Eq)",
            "data WorktreeSpec = WorktreeSpec { specSource :: WorktreeSource, specLabel :: Text, specDirtyPolicy :: DirtyPolicy } deriving (Show, Eq)",
            "data InProgressKind = InProgressMerge | InProgressRebase | InProgressCherryPick | InProgressRevert | InProgressBisect deriving (Show, Eq)",
            "data DirtySummary = DirtySummary { staged :: [Text], unstaged :: [Text], untracked :: [Text], ignoredExcluded :: Int } deriving (Show, Eq)",
            "data GitFailureReceipt = GitFailureReceipt { gitArgs :: [Text], gitCwd :: Text, gitExitCode :: Maybe Int, gitStdout :: Text, gitStderr :: Text } deriving (Show, Eq)",
            "data WorktreeReceipt = WorktreeReceipt { treeId :: WorktreeId, cwd :: Text, branch :: BranchName, sourceHead :: GitOid, snapshotRef :: Maybe GitRef, createdAt :: Int } deriving (Show, Eq)",
            "data WorktreeHandle = WorktreeHandle { handleReceipt :: WorktreeReceipt } deriving (Show, Eq)",
            "data WorktreeSummary = WorktreeSummary { summaryReceipt :: WorktreeReceipt, present :: Bool } deriving (Show, Eq)",
            // -- 14th `data` decl, added after the flip: `MergeOutcome` -------
            "data MergeOutcome = Merged GitOid | Conflict [Text] deriving (Show, Eq)",
            // -- 7 `instance ToJSON` lines, macro order -----------------------
            "instance ToJSON WorktreeId where toJSON (WorktreeId t) = toJSON t",
            "instance ToJSON GitOid where toJSON (GitOid t) = toJSON t",
            "instance ToJSON GitRef where toJSON (GitRef t) = toJSON t",
            "instance ToJSON BranchName where toJSON (BranchName t) = toJSON t",
            "instance ToJSON InProgressKind where toJSON k = toJSON (show k)",
            "instance ToJSON DirtySummary where toJSON d = object [\"staged\" .= d.staged, \"unstaged\" .= d.unstaged, \"untracked\" .= d.untracked, \"ignoredExcluded\" .= d.ignoredExcluded]",
            "instance ToJSON GitFailureReceipt where toJSON r = object [\"args\" .= r.gitArgs, \"cwd\" .= r.gitCwd, \"exitCode\" .= r.gitExitCode, \"stdout\" .= r.gitStdout, \"stderr\" .= r.gitStderr]",
            // -- 21st entry: the rendered `WorktreeError` ADT -----------------
            // `error_decl_text!` peels the first `errors` variant so `|`
            // separators land BETWEEN constructors, then hand-templates a
            // `ToJSON` instance (`error_variant_json_arm!`) because the
            // vendored `ToJSON`'s generic default only covers
            // single-constructor records.
            concat!(
                "data WorktreeError = SourceDirty DirtySummary | NotARepository Text | WorktreeLost WorktreeId | DirtySubmoduleUnsupported Text | SourceOperationInProgress InProgressKind | WorktreeBusy WorktreeId Text | GitFailure GitFailureReceipt | WorktreeNotRegistered WorktreeId | InvalidRegistryRoot Text Text | StorageFailure Text Text deriving (Show, Eq)\n",
                "instance ToJSON WorktreeError where\n",
                "  toJSON e = case e of\n",
                "    SourceDirty dirty -> object [\"tag\" .= (\"SourceDirty\" :: Text), \"dirty\" .= dirty]\n",
                "    NotARepository path -> object [\"tag\" .= (\"NotARepository\" :: Text), \"path\" .= path]\n",
                "    WorktreeLost lostId -> object [\"tag\" .= (\"WorktreeLost\" :: Text), \"lostId\" .= lostId]\n",
                "    DirtySubmoduleUnsupported submodule -> object [\"tag\" .= (\"DirtySubmoduleUnsupported\" :: Text), \"submodule\" .= submodule]\n",
                "    SourceOperationInProgress inProgress -> object [\"tag\" .= (\"SourceOperationInProgress\" :: Text), \"inProgress\" .= inProgress]\n",
                "    WorktreeBusy busyId holder -> object [\"tag\" .= (\"WorktreeBusy\" :: Text), \"busyId\" .= busyId, \"holder\" .= holder]\n",
                "    GitFailure receipt -> object [\"tag\" .= (\"GitFailure\" :: Text), \"receipt\" .= receipt]\n",
                "    WorktreeNotRegistered notRegisteredId -> object [\"tag\" .= (\"WorktreeNotRegistered\" :: Text), \"notRegisteredId\" .= notRegisteredId]\n",
                "    InvalidRegistryRoot root inside -> object [\"tag\" .= (\"InvalidRegistryRoot\" :: Text), \"root\" .= root, \"inside\" .= inside]\n",
                "    StorageFailure storagePath storageDetail -> object [\"tag\" .= (\"StorageFailure\" :: Text), \"storagePath\" .= storagePath, \"storageDetail\" .= storageDetail]\n",
            ),
        ]
    );
}

/// The 5 constructor signatures from the macro, worked out from `ctor_sig!`'s
/// errors-tagged arm (`<Ctor> :: <args> -> Worktree (Either WorktreeError
/// <ret>)`) applied to the macro's `verbs` rows, in the macro's verb order,
/// plus a 6th added after the flip: `WorktreeMergeInto`.
#[test]
fn worktree_constructor_signatures_are_pinned() {
    let wt = worktree();

    assert_eq!(
        wt.constructor_signatures(),
        vec![
            "WorktreeCreate :: WorktreeSpec -> Worktree (Either WorktreeError WorktreeHandle)",
            "WorktreeLookup :: WorktreeId -> Worktree (Either WorktreeError WorktreeHandle)",
            "WorktreeList :: Worktree (Either WorktreeError [WorktreeSummary])",
            "WorktreeBranchOf :: WorktreeId -> Worktree (Either WorktreeError BranchName)",
            "WorktreeHeadOf :: WorktreeId -> Worktree (Either WorktreeError GitOid)",
            "WorktreeMergeInto :: WorktreeId -> BranchName -> Text -> Worktree (Either WorktreeError MergeOutcome)",
        ]
    );
}

/// The four representable helpers — the three thin wrappers over one verb
/// (`createWorktree`, `lookupWorktree`, `listWorktrees`) plus the one pure
/// projection (`worktreeId`) — copied verbatim from the macro's `raw` lines,
/// newline-joined exactly as `helper_text!` joins them; plus a fifth, added
/// after the flip: `mergeBranchInto`, the thin wrapper over `WorktreeMergeInto`.
#[test]
fn worktree_helper_texts_are_pinned() {
    let wt = worktree();

    assert_eq!(
        wt.helper_texts(),
        vec![
            concat!(
                "-- | Create a managed worktree. `Left (SourceDirty summary)` when the\n",
                "-- source is dirty and the spec did not opt in; case-match the error\n",
                "-- rather than unwrapping if you mean to handle it.\n",
                "createWorktree :: forall effs. Member Worktree effs => WorktreeSpec -> Eff effs (Either WorktreeError WorktreeHandle)\n",
                "createWorktree = send . WorktreeCreate",
            ),
            concat!(
                "-- | Look a retained worktree up by durable id. Survives restart:\n",
                "-- resolution reads on-disk registry state, not process memory.\n",
                "-- `Left (WorktreeLost i)` when it is registered but gone from disk.\n",
                "lookupWorktree :: forall effs. Member Worktree effs => WorktreeId -> Eff effs (Either WorktreeError WorktreeHandle)\n",
                "lookupWorktree = send . WorktreeLookup",
            ),
            concat!(
                "-- | Every registered worktree, present or lost. A lost tree is listed\n",
                "-- with `present = False` rather than failing the whole listing.\n",
                "listWorktrees :: forall effs. Member Worktree effs => Eff effs [WorktreeSummary]\n",
                "listWorktrees = send WorktreeList >>= liftEither",
            ),
            concat!(
                "-- | The durable identity of a managed worktree. Pure: the handle\n",
                "-- already carries its receipt, so this reads no git state.\n",
                "worktreeId :: WorktreeHandle -> WorktreeId\n",
                "worktreeId h = h.handleReceipt.treeId",
            ),
            concat!(
                "-- | Merge `branch` into the worktree `treeId` names, as `git merge --no-ff`\n",
                "-- — never a fast-forward, so a landed merge always carries a genuine merge\n",
                "-- commit. On conflict, the conflicting paths are read and the merge is\n",
                "-- ABORTED before this returns: the worktree is left clean either way,\n",
                "-- success or conflict. `Left (GitFailure r)` is a `git` invocation that\n",
                "-- never entered a merge at all (an unknown branch, a locked index) —\n",
                "-- distinct from `Right (Conflict paths)`, a merge that genuinely started\n",
                "-- and conflicted.\n",
                "mergeBranchInto :: forall effs. Member Worktree effs => WorktreeId -> BranchName -> Text -> Eff effs (Either WorktreeError MergeOutcome)\n",
                "mergeBranchInto treeId branch message = send (WorktreeMergeInto treeId branch message)",
            ),
        ]
    );
}

/// The honest record of the gap: ten of the macro's fourteen helpers are NOT
/// representable, so they are not in the schema at all — excluded rather than
/// smuggled in as raw strings. They are
/// DEFINITIONS in `haskell/lib/Tidepool/Worktree.hs`, reached from an eval
/// through this effect's `extra_imports` row. This is a SEPARATE test from the
/// pinned four above because it asserts an absence, not a rendering.
///
/// Why each of the ten is unrepresentable:
/// - `fromCurrentRepository`, `fromRef`, `fromWorktree`, `allowDirtySnapshot`
///   — four build (or update) a `WorktreeSpec` purely; three of those
///   construct one from scratch and the fourth (`allowDirtySnapshot`) is a
///   record update, none of which is a call to `send`.
/// - `worktreeBranch`, `worktreeHead` — each adapts its argument through
///   `worktreeId h` before sending, so the wrapper is not thin over the verb's
///   own argument.
/// - `renderWorktreeId`, `renderGitOid`, `renderBranchName` — each unwraps an
///   identity newtype; no `send` involved.
/// - `renderWorktreeError` — a ten-arm string-formatting program over
///   `WorktreeError`'s variants, the largest of the ten.
///
/// `worktreeId` is NOT here, and its absence from this list is worth noting:
/// it is a field-projection chain rather than a verb call, so it was
/// originally counted among the relocations — but the generated module's own
/// RepoEvent helpers call it, and that module cannot import
/// `Tidepool.Worktree`. It is represented as
/// `HelperBody::Projection` instead, which is why the count here is ten.
///
/// 4 representable + 10 unrepresentable = 14, the macro's total helper count
/// — plus `mergeBranchInto`, a 5th representable helper added after the flip
/// (not one of the macro's original fourteen), for 15 total.
#[test]
fn worktree_ten_helpers_are_not_schema_representable() {
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
    assert_eq!(NOT_REPRESENTABLE.len(), 10);
    assert_eq!(NOT_REPRESENTABLE.len() + worktree().helpers.len(), 15);

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
        body: HelperBody::Pointfree,
    }];
    let errs = eff
        .validate()
        .expect_err("a send-wrapper must name the verb it wraps");
    assert!(
        errs.iter()
            .any(|e| e.contains("names no verb, but only a pure projection may")),
        "got: {errs:?}"
    );
}
