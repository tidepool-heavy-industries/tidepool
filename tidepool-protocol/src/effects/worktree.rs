//! Canonical schema for the Worktree effect.
//!
//! One ordered declaration produces the Haskell vocabulary, Rust wire types,
//! request decoder, and handler dispatch. Pure helpers that the schema can
//! represent live here; richer authored helpers live in
//! `haskell/lib/Tidepool/Worktree.hs` and are imported through `extra_imports`.
//! `WorktreeTryMerge` is the deliberately narrow workflow exception; other
//! Git workflow and conflict resolution remain native-agent work.

use crate::hs::HsType;
use crate::schema::{
    AdapterKind, Arg, DomainMap, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper,
    HelperBody, IdentityPayload, JsonInstance, OuterEffect, Polymorphism, RecordField, RustBinding,
    SumVariant, TypeDef, TypeShape, Validation, VariantFields, Verb, WireDerives,
};
use crate::types::WireDerive::{
    Clone as DClone, Copy as DCopy, Debug as DDebug, Default as DDefault, Eq as DEq,
    FromCore as DFromCore, PartialEq as DPartialEq, ToCore as DToCore,
};

/// The derive set every Worktree wire type shares.
const WIRE: WireDerives = WireDerives(&[DToCore, DFromCore, DClone, DDebug, DPartialEq, DEq]);
/// …plus `Copy`, for the two payload-free sums.
const WIRE_COPY: WireDerives =
    WireDerives(&[DToCore, DFromCore, DClone, DCopy, DDebug, DPartialEq, DEq]);
/// …plus `Default`, for `DirtySummary` (a clean tree is the empty summary).
const WIRE_DEFAULT: WireDerives = WireDerives(&[
    DToCore, DFromCore, DClone, DDebug, DDefault, DPartialEq, DEq,
]);

/// `data X = X Text` with a `WorktreeId`-shaped path-safety policy.
fn identity(
    name: &'static str,
    wire_rust: &'static str,
    validation: Validation,
    doc: &'static [&'static str],
    domain_path: &'static str,
    from_wire: Option<AdapterKind>,
) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(wire_rust),
        shape: TypeShape::Identity {
            payload: IdentityPayload::Text,
            hs_binder: "t",
            rust_field: "raw",
            validation,
        },
        // The identity types render as their bare payload: a receipt reader
        // wants the id, not a wrapper object.
        json: JsonInstance::Transparent,
        derives: WIRE,
        domain: Some(DomainMap {
            domain_path,
            into_wire: Some(AdapterKind::IdentityRaw {
                as_str: "as_str",
                from_raw: "from_raw",
            }),
            from_wire,
        }),
        doc,
    }
}

/// The Worktree effect, completely.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn worktree() -> Effect {
    Effect {
        name: "Worktree",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "WorktreeHandler",
        handler_module: "worktree",
        req_enum: "WorktreeReq",
        decl_fn: "worktree_decl",
        description: &[
            "Managed git worktrees: create an isolated worktree from a clean source ",
            "(or, explicitly, from a dirty-source snapshot), look a retained one back ",
            "up by durable id, and list what exists. Retain-first — there is no ",
            "release or delete verb in v1, deliberately. Git WORKFLOW (rebase, merge, ",
            "cherry-pick, conflict resolution) is absent by design: that work belongs ",
            "to coding agents with their native tools, and Tidepool observes what the ",
            "repository became through `Tidepool.Event`.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        // Rich authored helpers are defined in the library module. A row
        // carrying Worktree imports that module, while generated helpers are
        // re-exported from the same place so each name has one public origin.
        extra_imports: &["import Tidepool.Worktree"],
        type_defs: type_defs(),
        foreign_types: &[],
        // Worktree failures are authored data, not eval aborts. The
        // domain-to-wire map remains hand-written because several variants
        // differ in representation and therefore carry mapping decisions.
        errors: Some(errors()),
        verbs: verbs(),
        helpers: helpers(),
        polymorphism: Polymorphism::None,
        dispatched: true,
    }
}

/// Supporting declarations in their canonical wire-emission order.
fn type_defs() -> Vec<TypeDef> {
    vec![
        identity(
            "WorktreeId",
            "WtWorktreeId",
            // Exactly `tidepool_worktree::WorktreeId::is_path_safe`, hoisted out
            // of the handler's hand-written `worktree_id_from_wire` into
            // declared data. Ids are joined into registry/binding file paths as
            // a SINGLE component, so a wire value carrying a separator is a path
            // escape, not a lookup miss.
            Validation::Segment {
                max_len: 128,
                extra_allowed: "-_",
            },
            &[
                "Haskell `WorktreeId` — opaque durable identity. `data`, not a synonym: PRD",
                "19 requires that a `GitOid` can never be passed where a worktree id is",
                "wanted.",
            ],
            "tidepool_worktree::WorktreeId",
            // The handler keeps the SEMANTIC half: a rejected id is spelled
            // `WorktreeNotRegistered`, because no id outside the minted alphabet
            // was ever registered and the caller learns nothing about the
            // filesystem. Only the mechanical check is generated.
            Some(AdapterKind::HandWritten(
                "the rejection must become a DOMAIN error (`WorktreeNotRegistered`); only \
                 the path-safety check is generated, as this type's boundary constructor",
            )),
        ),
        identity(
            "GitOid",
            "WtGitOid",
            Validation::NonEmpty,
            &["Haskell `GitOid` — domain data, distinct from `EvEventId`'s runtime identity."],
            "tidepool_worktree::GitOid",
            // No verb accepts a GitOid FROM Haskell, so no wire→domain
            // conversion exists to generate or hand-write.
            None,
        ),
        identity(
            "GitRef",
            "WtGitRef",
            Validation::NonEmpty,
            &["Haskell `GitRef` — a branch, tag, remote ref, or raw OID, resolved by git."],
            "tidepool_worktree::GitRef",
            Some(AdapterKind::IdentityRaw {
                as_str: "as_str",
                from_raw: "from_raw",
            }),
        ),
        identity(
            "BranchName",
            "WtBranchName",
            Validation::NonEmpty,
            &["Haskell `BranchName` — stored without the `refs/heads/` prefix."],
            "tidepool_worktree::BranchName",
            // `WorktreeTryMerge` can carry a `BranchName` as readable
            // provenance, so this direction is exercised — infallible, same
            // as `GitRef`'s.
            Some(AdapterKind::IdentityRaw {
                as_str: "as_str",
                from_raw: "from_raw",
            }),
        ),
        TypeDef {
            name: "WorktreeSource",
            wire_rust: Some("WtWorktreeSource"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "SourceCurrentRepository",
                        fields: positional_fields![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "SourceRef",
                        fields: positional_fields![HsType::Named("GitRef")],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "SourceWorktree",
                        fields: positional_fields![HsType::Named("WorktreeId")],
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::WorktreeSource",
                into_wire: None,
                from_wire: Some(AdapterKind::HandWritten(
                    "composes a FALLIBLE conversion (`worktree_id_from_wire`); the error \
                     path is semantic",
                )),
            }),
            doc: &["Haskell `WorktreeSource`."],
        },
        TypeDef {
            name: "DirtyPolicy",
            wire_rust: Some("WtDirtyPolicy"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "RequireClean",
                        fields: positional_fields![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "AllowDirtySnapshot",
                        fields: positional_fields![],
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE_COPY,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::DirtyPolicy",
                into_wire: None,
                from_wire: Some(AdapterKind::VariantMap(&[
                    ("RequireClean", "RequireClean"),
                    ("AllowDirtySnapshot", "AllowDirtySnapshot"),
                ])),
            }),
            doc: &[
                "Haskell `DirtyPolicy`. Clean-by-default is the safety property; the opt-in",
                "is spelled at the authored call site.",
            ],
        },
        TypeDef {
            name: "WorktreeSpec",
            wire_rust: Some("WtWorktreeSpec"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "specSource",
                        rust_name: "spec_source",
                        ty: HsType::Named("WorktreeSource"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "specLabel",
                        rust_name: "spec_label",
                        ty: HsType::Text,
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "specDirtyPolicy",
                        rust_name: "spec_dirty_policy",
                        ty: HsType::Named("DirtyPolicy"),
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::WorktreeSpec",
                into_wire: None,
                from_wire: Some(AdapterKind::HandWritten(
                    "composes a FALLIBLE conversion; the error path is semantic",
                )),
            }),
            doc: &["Haskell `WorktreeSpec` — built in Haskell, consumed in Rust."],
        },
        TypeDef {
            name: "InProgressKind",
            wire_rust: Some("WtInProgressKind"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "InProgressMerge",
                        fields: positional_fields![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "InProgressRebase",
                        fields: positional_fields![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "InProgressCherryPick",
                        fields: positional_fields![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "InProgressRevert",
                        fields: positional_fields![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "InProgressBisect",
                        fields: positional_fields![],
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::ShownString { binder: "k" },
            derives: WIRE_COPY,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::InProgressKind",
                // Renames EVERY variant. This is the mechanical-but-error-prone
                // case the generator earns its keep on.
                into_wire: Some(AdapterKind::VariantMap(&[
                    ("Merge", "InProgressMerge"),
                    ("Rebase", "InProgressRebase"),
                    ("CherryPick", "InProgressCherryPick"),
                    ("Revert", "InProgressRevert"),
                    ("Bisect", "InProgressBisect"),
                ])),
                from_wire: None,
            }),
            doc: &[
                "Haskell `InProgressKind` — distinguished rather than collapsed to a string",
                "so a resident can branch on it.",
            ],
        },
        TypeDef {
            name: "DirtySummary",
            wire_rust: Some("WtDirtySummary"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "staged",
                        rust_name: "staged",
                        ty: HsType::list(HsType::Text),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "unstaged",
                        rust_name: "unstaged",
                        ty: HsType::list(HsType::Text),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "untracked",
                        rust_name: "untracked",
                        ty: HsType::list(HsType::Text),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "ignoredExcluded",
                        rust_name: "ignored_excluded",
                        ty: HsType::Int,
                        doc: &[],
                    },
                ],
            },
            // Keys equal the field names here — spelled anyway, because
            // `JsonInstance::Object` requires every field and a partial object
            // is a silent omission.
            json: JsonInstance::Object {
                binder: "d",
                keys: &[
                    ("staged", "staged"),
                    ("unstaged", "unstaged"),
                    ("untracked", "untracked"),
                    ("ignoredExcluded", "ignoredExcluded"),
                ],
            },
            derives: WIRE_DEFAULT,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::DirtySummary",
                into_wire: Some(AdapterKind::HandWritten(
                    "`usize` → `i64` widening on `ignoredExcluded`, and the three lists are \
                     cloned out of a borrow",
                )),
                from_wire: None,
            }),
            doc: &[
                "Haskell `DirtySummary`. `ignored_excluded` is a COUNT, not a list: ignored",
                "files are deliberately excluded from a snapshot, and listing them invites an",
                "author to believe they were captured.",
            ],
        },
        TypeDef {
            name: "HeadState",
            wire_rust: Some("WtHeadState"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "OnBranch",
                        fields: VariantFields::Named(vec![
                            RecordField {
                                hs_name: "headBranch",
                                rust_name: "branch",
                                ty: HsType::Named("BranchName"),
                                doc: &["The checked-out branch."],
                            },
                            RecordField {
                                hs_name: "headOid",
                                rust_name: "oid",
                                ty: HsType::Named("GitOid"),
                                doc: &["The commit checked out at observation time."],
                            },
                        ]),
                        doc: &["The checkout is attached to this branch at this commit."],
                    },
                    SumVariant {
                        ctor: "Detached",
                        fields: VariantFields::Named(vec![RecordField {
                            hs_name: "headOid",
                            rust_name: "oid",
                            ty: HsType::Named("GitOid"),
                            doc: &["The detached commit checked out at observation time."],
                        }]),
                        doc: &["The checkout has a detached HEAD at this commit."],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::HeadState",
                into_wire: Some(AdapterKind::HandWritten(
                    "both variants wrap domain identity values into generated wire identities",
                )),
                from_wire: None,
            }),
            doc: &["The checked-out identity at submission-observation time."],
        },
        TypeDef {
            name: "WorkingState",
            wire_rust: Some("WtWorkingState"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "changes",
                        rust_name: "changes",
                        ty: HsType::Named("DirtySummary"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "operation",
                        rust_name: "operation",
                        ty: HsType::maybe(HsType::Named("InProgressKind")),
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::WorkingState",
                into_wire: Some(AdapterKind::HandWritten(
                    "composes DirtySummary and optional InProgressKind conversions",
                )),
                from_wire: None,
            }),
            doc: &["Mutable repository state observed with a submitted HEAD."],
        },
        TypeDef {
            name: "SubmissionObservation",
            wire_rust: Some("WtSubmissionObservation"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "observedWorktreeId",
                        rust_name: "observed_worktree_id",
                        ty: HsType::Named("WorktreeId"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "baseHead",
                        rust_name: "base_head",
                        ty: HsType::Named("GitOid"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "committedPaths",
                        rust_name: "committed_paths",
                        ty: HsType::List(Box::new(HsType::Text)),
                        doc: &["Paths changed by commits after the request base."],
                    },
                    RecordField {
                        hs_name: "submittedHead",
                        rust_name: "submitted_head",
                        ty: HsType::Named("HeadState"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "workingState",
                        rust_name: "working_state",
                        ty: HsType::Named("WorkingState"),
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::SubmissionObservation",
                into_wire: Some(AdapterKind::HandWritten(
                    "composes the generated identity, head-state, and working-state conversions",
                )),
                from_wire: None,
            }),
            doc: &[
                "One bounded, internally stable observation of a candidate checkout.",
                "This is not a seal: dirty state is evidence, while a clean commit OID is",
                "the immutable integration artifact.",
            ],
        },
        TypeDef {
            name: "GitFailureReceipt",
            wire_rust: Some("WtGitFailureReceipt"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "gitArgs",
                        rust_name: "git_args",
                        ty: HsType::list(HsType::Text),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "gitCwd",
                        rust_name: "git_cwd",
                        ty: HsType::Text,
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "gitExitCode",
                        rust_name: "git_exit_code",
                        ty: HsType::maybe(HsType::Int),
                        doc: &["`None` when the process was killed by a signal before exiting."],
                    },
                    RecordField {
                        hs_name: "gitStdout",
                        rust_name: "git_stdout",
                        ty: HsType::Text,
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "gitStderr",
                        rust_name: "git_stderr",
                        ty: HsType::Text,
                        doc: &[],
                    },
                ],
            },
            // Keys are DELIBERATELY renamed, so the JSON reads as a git receipt
            // rather than as a struct dump. The rename is data here instead of
            // being buried in a source string.
            json: JsonInstance::Object {
                binder: "r",
                keys: &[
                    ("args", "gitArgs"),
                    ("cwd", "gitCwd"),
                    ("exitCode", "gitExitCode"),
                    ("stdout", "gitStdout"),
                    ("stderr", "gitStderr"),
                ],
            },
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::GitFailureReceipt",
                into_wire: Some(AdapterKind::HandWritten(
                    "`PathBuf` → lossy `String`, `Option<i32>` → `Option<i64>`",
                )),
                from_wire: None,
            }),
            doc: &[
                "Haskell `GitFailureReceipt` — a failed git invocation, recorded verbatim so",
                "the failure is diagnosable without re-running anything. Keeps stdout AND",
                "stderr, never just the status.",
            ],
        },
        TypeDef {
            name: "WorktreeReceipt",
            wire_rust: Some("WtWorktreeReceipt"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "treeId",
                        rust_name: "tree_id",
                        ty: HsType::Named("WorktreeId"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "cwd",
                        rust_name: "cwd",
                        ty: HsType::Text,
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "branch",
                        rust_name: "branch",
                        ty: HsType::Named("BranchName"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "sourceHead",
                        rust_name: "source_head",
                        ty: HsType::Named("GitOid"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "snapshotRef",
                        rust_name: "snapshot_ref",
                        ty: HsType::maybe(HsType::Named("GitRef")),
                        doc: &[],
                    },
                    // NOT promoted to a typed timestamp: promoting `createdAt ::
                    // Int` would change the Haskell declaration, and that
                    // declaration is byte-locked by the Class A goldens.
                    RecordField {
                        hs_name: "createdAt",
                        rust_name: "created_at",
                        ty: HsType::Int,
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::WorktreeReceipt",
                into_wire: Some(AdapterKind::HandWritten(
                    "field renames (`worktree_id`→`tree_id`, `created_at_ms`→`created_at`) \
                     plus a `PathBuf` → lossy `String`",
                )),
                from_wire: None,
            }),
            doc: &[
                "Haskell `WorktreeReceipt`. The id field is `tree_id`/`treeId` rather than",
                "the PRD snippet's `worktreeId` — a record field selector and the top-level",
                "`worktreeId` function the PRD pins by signature would be an ambiguous",
                "occurrence at the export, so the field yielded.",
            ],
        },
        TypeDef {
            name: "WorktreeHandle",
            wire_rust: Some("WtWorktreeHandle"),
            shape: TypeShape::Record {
                fields: vec![RecordField {
                    hs_name: "handleReceipt",
                    rust_name: "handle_receipt",
                    ty: HsType::Named("WorktreeReceipt"),
                    doc: &[],
                }],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::WorktreeHandle",
                into_wire: Some(AdapterKind::HandWritten(
                    "delegates to the hand-written receipt conversion through `h.receipt()`",
                )),
                from_wire: None,
            }),
            doc: &[
                "Haskell `WorktreeHandle` — a name plus its recorded facts, not an open",
                "handle to anything.",
            ],
        },
        TypeDef {
            name: "WorktreeSummary",
            wire_rust: Some("WtWorktreeSummary"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "summaryReceipt",
                        rust_name: "summary_receipt",
                        ty: HsType::Named("WorktreeReceipt"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "present",
                        rust_name: "present",
                        ty: HsType::Bool,
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::WorktreeSummary",
                into_wire: Some(AdapterKind::HandWritten(
                    "delegates to the hand-written receipt conversion",
                )),
                from_wire: None,
            }),
            doc: &[
                "Haskell `WorktreeSummary`. `present` is a filesystem fact re-derived on each",
                "listing rather than a recorded one, so a lost tree is listed rather than",
                "failing the listing.",
            ],
        },
        TypeDef {
            name: "MergeRequest",
            wire_rust: Some("WtMergeRequest"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "mergeSourceHead",
                        rust_name: "source_head",
                        ty: HsType::Named("GitOid"),
                        doc: &["The exact observed source commit; this, not the branch label, is merged."],
                    },
                    RecordField {
                        hs_name: "mergeSourceBranch",
                        rust_name: "source_branch",
                        ty: HsType::maybe(HsType::Named("BranchName")),
                        doc: &["Optional readable provenance. If present, it must still resolve to sourceHead."],
                    },
                    RecordField {
                        hs_name: "mergeTargetWorktree",
                        rust_name: "target_worktree",
                        ty: HsType::Named("WorktreeId"),
                        doc: &["The managed worktree whose checked-out branch receives the merge."],
                    },
                    RecordField {
                        hs_name: "mergeMessage",
                        rust_name: "merge_message",
                        ty: HsType::Text,
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &[
                "A conservative merge request with named source and target roles.",
                "The exact source OID prevents a retained child branch moving between review and fold.",
            ],
        },
        TypeDef {
            name: "MergeOutcome",
            wire_rust: Some("WtMergeOutcome"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "AlreadyContained",
                        fields: positional_fields![HsType::Named("GitOid"), HsType::Named("GitOid")],
                        doc: &["The source was already reachable from the target; no mutation occurred."],
                    },
                    SumVariant {
                        ctor: "FastForwarded",
                        fields: positional_fields![HsType::Named("GitOid"), HsType::Named("GitOid"), HsType::Named("GitOid")],
                        doc: &["The target moved directly from before to the source commit."],
                    },
                    SumVariant {
                        ctor: "CreatedMergeCommit",
                        fields: positional_fields![HsType::Named("GitOid"), HsType::Named("GitOid"), HsType::Named("GitOid")],
                        doc: &["Divergent histories produced a new merge commit."],
                    },
                    SumVariant {
                        ctor: "ManualGitRequired",
                        fields: positional_fields![HsType::Named("GitOid"), HsType::Named("GitOid"), HsType::Text, HsType::list(HsType::Text)],
                        doc: &["Automatic integration stopped cleanly. The target is restored; use ordinary Git."],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::merge::MergeOutcome",
                into_wire: Some(AdapterKind::HandWritten(
                    "each outcome wraps domain Git OIDs and the manual handoff also clones its path Vec",
                )),
                from_wire: None,
            }),
            doc: &[
                "Haskell `MergeOutcome` — the result of merging one branch into a target",
                "worktree via `tryMerge`. A `git` invocation failure that never",
                "entered a merge at all (an unknown branch, a locked index) is NOT this —",
                "it surfaces as `Left (GitFailure _)` instead; this type only ever describes",
                "a merge that actually started.",
            ],
        },
    ]
}

/// One error field, spelled as the Rust wire type the handler receives.
fn field(name: &'static str, hs: &'static str, wire: &'static str) -> ErrorField {
    ErrorField {
        name,
        ty: HsType::Named(hs),
        rust: RustBinding::Bridged(wire),
    }
}

/// A `Text` error field.
fn text_field(name: &'static str) -> ErrorField {
    ErrorField {
        name,
        ty: HsType::Text,
        rust: RustBinding::Derived,
    }
}

fn errors() -> ErrorAdt {
    ErrorAdt {
        name: "WorktreeError",
        variants: vec![
            ErrorVariant {
                ctor: "SourceDirty",
                fields: vec![field("dirty", "DirtySummary", "WtDirtySummary")],
                doc: "source working tree has uncommitted state and the spec did not opt into a snapshot",
            },
            ErrorVariant {
                ctor: "NotARepository",
                fields: vec![text_field("path")],
                doc: "the path is not inside a git repository",
            },
            ErrorVariant {
                ctor: "WorktreeLost",
                fields: vec![field("lostId", "WorktreeId", "WtWorktreeId")],
                doc: "registered but gone from disk; never silently recreated",
            },
            ErrorVariant {
                ctor: "DirtySubmoduleUnsupported",
                fields: vec![text_field("submodule")],
                doc: "a dirty submodule in the source; v1 refuses rather than capturing a gitlink it did not follow",
            },
            ErrorVariant {
                ctor: "SourceOperationInProgress",
                fields: vec![field("inProgress", "InProgressKind", "WtInProgressKind")],
                doc: "source is mid-merge / mid-rebase / mid-cherry-pick",
            },
            ErrorVariant {
                ctor: "WorktreeBusy",
                fields: vec![
                    field("busyId", "WorktreeId", "WtWorktreeId"),
                    text_field("holder"),
                ],
                doc: "one worktree, one agent — binding a second fails explicitly",
            },
            ErrorVariant {
                ctor: "SubmissionUnstable",
                fields: vec![field("unstableId", "WorktreeId", "WtWorktreeId")],
                doc: "the checkout kept changing during bounded submission observation",
            },
            ErrorVariant {
                ctor: "WorktreeUnauthorized",
                fields: vec![field("unauthorizedId", "WorktreeId", "WtWorktreeId")],
                doc: "the executing principal has no active binding for this managed worktree",
            },
            ErrorVariant {
                ctor: "WorktreeAuthorityDenied",
                fields: vec![text_field("authorityDetail")],
                doc: "the executing principal's actor role does not permit this Worktree operation",
            },
            ErrorVariant {
                ctor: "GitFailure",
                fields: vec![field("receipt", "GitFailureReceipt", "WtGitFailureReceipt")],
                doc: "git itself failed; the receipt carries the invocation and its output",
            },
            ErrorVariant {
                ctor: "WorktreeNotRegistered",
                fields: vec![field("notRegisteredId", "WorktreeId", "WtWorktreeId")],
                doc: "no worktree registered under this id — a typo or a stale id, DISTINCT from WorktreeLost's data loss",
            },
            ErrorVariant {
                ctor: "InvalidRegistryRoot",
                fields: vec![text_field("root"), text_field("inside")],
                doc: "the registry root resolves inside a git working tree; it must live outside every source repository",
            },
            ErrorVariant {
                ctor: "StorageFailure",
                fields: vec![text_field("storagePath"), text_field("storageDetail")],
                doc: "I/O failure against Tidepool's own registry / binding table / journal",
            },
        ],
    }
}

fn verbs() -> Vec<Verb> {
    let handle = HsType::Named("WorktreeHandle");
    vec![
        Verb {
            ctor: "WorktreeCreate",
            method: "worktree_create",
            args: vec![Arg {
                name: "spec",
                ty: HsType::Named("WorktreeSpec"),
                rust: RustBinding::Bridged("WtWorktreeSpec"),
            }],
            ret: handle.clone(),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        Verb {
            ctor: "WorktreeCreateForActorPath",
            method: "worktree_create_for_actor_path",
            args: vec![
                Arg {
                    name: "spec",
                    ty: HsType::Named("WorktreeSpec"),
                    rust: RustBinding::Bridged("WtWorktreeSpec"),
                },
                Arg {
                    name: "actorPath",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
            ],
            ret: handle.clone(),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        Verb {
            ctor: "WorktreeCreateFromBoundForActorPath",
            method: "worktree_create_from_bound_for_actor_path",
            args: vec![
                Arg {
                    name: "dirtyPolicy",
                    ty: HsType::Named("DirtyPolicy"),
                    rust: RustBinding::Bridged("WtDirtyPolicy"),
                },
                Arg {
                    name: "actorPath",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
            ],
            ret: handle.clone(),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        Verb {
            ctor: "WorktreeLookup",
            method: "worktree_lookup",
            args: vec![tree_id_arg()],
            ret: handle.clone(),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        Verb {
            ctor: "WorktreeBound",
            method: "worktree_bound",
            args: vec![],
            ret: handle.clone(),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        Verb {
            ctor: "WorktreeList",
            method: "worktree_list",
            args: vec![],
            ret: HsType::list(HsType::Named("WorktreeSummary")),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        Verb {
            ctor: "WorktreeListMatching",
            method: "worktree_list_matching",
            args: vec![
                Arg {
                    name: "present",
                    ty: HsType::maybe(HsType::Bool),
                    rust: RustBinding::Path("Option<bool>"),
                },
                Arg {
                    name: "branchPrefix",
                    ty: HsType::maybe(HsType::Text),
                    rust: RustBinding::Path("Option<String>"),
                },
                Arg {
                    name: "createdAfter",
                    ty: HsType::maybe(HsType::Int),
                    rust: RustBinding::Path("Option<i64>"),
                },
            ],
            ret: HsType::list(HsType::Named("WorktreeSummary")),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        Verb {
            ctor: "WorktreeBranchOf",
            method: "worktree_branch_of",
            args: vec![tree_id_arg()],
            ret: HsType::Named("BranchName"),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        // A FRESH git read of the worktree's current HEAD. Deliberately NOT the
        // handle's recorded `sourceHead` (the seed the branch was rooted at) and
        // NOT the monitor's last-observed baseline: the entire point is to see
        // what the monitor did not.
        Verb {
            ctor: "WorktreeHeadOf",
            method: "worktree_head_of",
            args: vec![tree_id_arg()],
            ret: HsType::Named("GitOid"),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        Verb {
            ctor: "WorktreeObserveSubmission",
            method: "worktree_observe_submission",
            args: vec![tree_id_arg()],
            ret: HsType::Named("SubmissionObservation"),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
        // The one narrow, deliberate workflow primitive (the
        // worktree-coordination fold — see `tidepool-worktree/src/merge.rs`
        // and this crate's `CLAUDE.md`): merge one branch into a target
        // worktree, typed and classified ONCE, so authored harnesses stop
        // re-deriving conflict-vs-failure over raw `Exec`. It is not a
        // general git-workflow surface — rebase, cherry-pick, and conflict
        // RESOLUTION are still absent by design and still authored policy.
        Verb {
            ctor: "WorktreeTryMerge",
            method: "worktree_try_merge",
            args: vec![Arg {
                name: "request",
                ty: HsType::Named("MergeRequest"),
                rust: RustBinding::Bridged("WtMergeRequest"),
            }],
            ret: HsType::Named("MergeOutcome"),
            errors: Some("WorktreeError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::Worktree),
            extract: None,
        },
    ]
}

fn tree_id_arg() -> Arg {
    Arg {
        name: "treeId",
        ty: HsType::Named("WorktreeId"),
        rust: RustBinding::Bridged("WtWorktreeId"),
    }
}

/// Helpers representable as thin verb wrappers or pure projections. Richer
/// authored helpers live in `Tidepool.Worktree`; `extra_imports` keeps the
/// combined surface transparent to callers.
fn helpers() -> Vec<Helper> {
    vec![
        Helper {
            name: "createWorktree",
            ctor: Some("WorktreeCreate"),
            substrate: false,
            doc: &[
                "Create a managed worktree. `Left (SourceDirty summary)` when the",
                "source is dirty and the spec did not opt in; case-match the error",
                "rather than unwrapping if you mean to handle it.",
            ],
            body: HelperBody::Pointfree,
        },
        Helper {
            name: "lookupWorktree",
            ctor: Some("WorktreeLookup"),
            substrate: false,
            doc: &[
                "Look a retained worktree up by durable id. Survives restart:",
                "resolution reads on-disk registry state, not process memory.",
                "`Left (WorktreeLost i)` when it is registered but gone from disk.",
            ],
            body: HelperBody::Pointfree,
        },
        Helper {
            name: "boundWorktree",
            ctor: Some("WorktreeBound"),
            substrate: false,
            doc: &[
                "Observe the managed worktree bound to the executing actor.",
                "This is custody lookup, not ambient current-directory inference.",
            ],
            body: HelperBody::Nullary,
        },
        Helper {
            name: "listWorktrees",
            ctor: Some("WorktreeList"),
            substrate: false,
            doc: &[
                "Every registered worktree, present or lost. A lost tree is listed",
                "with `present = False` rather than failing the whole listing.",
            ],
            body: HelperBody::Nullary,
        },
        Helper {
            name: "worktreeId",
            ctor: None,
            substrate: false,
            doc: &[
                "The durable identity of a managed worktree. Pure: the handle",
                "already carries its receipt, so this reads no git state.",
            ],
            body: HelperBody::Projection {
                binder: "h",
                arg: HsType::Named("WorktreeHandle"),
                // `WorktreeId` is DERIVED from here — `WorktreeHandle`'s
                // `handleReceipt` is a `WorktreeReceipt`, whose `treeId` is a
                // `WorktreeId`. Retyping either field moves this signature with
                // it instead of letting the two disagree.
                fields: &["handleReceipt", "treeId"],
            },
        },
        Helper {
            name: "tryMerge",
            ctor: Some("WorktreeTryMerge"),
            substrate: false,
            doc: &[
                "Attempt the ordinary merge named by a `MergeRequest`.",
                "The exact source commit is authoritative; an optional source branch is checked",
                "for drift. Straight-line history fast-forwards, divergent history creates a",
                "merge commit, and conflicts return `ManualGitRequired` only after aborting and",
                "proving the target returned to its starting HEAD and operation state.",
            ],
            body: HelperBody::Pointfree,
        },
        Helper {
            name: "observeSubmission",
            ctor: Some("WorktreeObserveSubmission"),
            substrate: false,
            doc: &[
                "Observe a candidate checkout through one bounded Worktree operation.",
                "This reports submitted HEAD, dirty state, and in-progress operation",
                "together; it does not seal or mutate the checkout.",
            ],
            body: HelperBody::Pointfree,
        },
    ]
}
