//! The Worktree effect — the first effect with a real record vocabulary.
//!
//! Lanes 1 and 2 (Exec, Journal) migrated effects whose `type_defs` were EMPTY.
//! Worktree carries thirteen supporting declarations, seven `ToJSON` instances,
//! an eleven-variant error ADT, and thirteen matching Rust wire structs that are
//! hand-written in `tidepool-bridge-effects` today with a comment asserting that
//! the two lists agree positionally. This module is where that comment goes to
//! die: one ordered [`crate::types::RecordField`] list produces both sides.
//!
//! **Flipped:** `worktree_effect_def!` is deleted, the
//! hand-written `Wt*` block is deleted, and with it the comment asserting that
//! the two field lists agree positionally — there is only one list now.
//!
//! **Helper representability.** Fourteen helpers lived in the hand-written
//! registry, all fourteen using the `raw` escape hatch. FOUR are described here:
//! three thin wrappers over one verb, plus `worktreeId`, the one pure projection
//! ([`crate::schema::HelperBody::Projection`]). The other ten are NOT smuggled
//! in as strings — they are DEFINITIONS in `haskell/lib/Tidepool/Worktree.hs`,
//! reachable from an eval through this effect's `extra_imports` row. That gap
//! is a real finding, not a shortfall of effort: the alternative was embedding a Haskell
//! expression language in the schema, which is the hatch under a different name.
//!
//! Two of the ten could not simply move: a helper emitted into the generated
//! `Tidepool.Effects` is in scope for every OTHER effect's helpers there, and
//! that module cannot import the library layer. `worktreeId` was made
//! representable; `renderWorktreeError`'s CALLER moved instead. Before
//! relocating any helper, grep every `*_effect_def!` for its name.
//!
//! **A fifth verb landed later, outside the fourteen-item census above.**
//! `WorktreeMergeInto`/`mergeBranchInto` exposes `tidepool_worktree::merge::
//! merge_branch_into` (one deliberate git-workflow exception — see
//! `tidepool-worktree/src/merge.rs`) as a typed verb, replacing two
//! authored Haskell reimplementations that had drifted from the canonical
//! conflict/failure classification (`harness-dogfooding/dev-tree` and
//! `/recursive-companion`'s own `mergeChild`/`mergeChildInto`).

use crate::hs::HsType;
use crate::schema::{
    AdapterKind, Arg, DomainMap, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper,
    HelperBody, IdentityPayload, JsonInstance, OuterEffect, Polymorphism, RecordField, RustBinding,
    SumVariant, TypeDef, TypeShape, Validation, Verb, WireDerives,
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
        // Eleven of the fourteen authored names are not schema-representable
        // (see the module doc) and are DEFINED in
        // `haskell/lib/Tidepool/Worktree.hs`. This row is what makes that
        // relocation invisible to an eval author: a row carrying Worktree
        // imports that module, so all fourteen names resolve exactly as they
        // did when the generated `Tidepool.Effects` defined them. The three
        // helpers below are re-exported by the same module, so they resolve to
        // one Name and cannot be an ambiguous occurrence.
        extra_imports: &["import Tidepool.Worktree"],
        type_defs: type_defs(),
        foreign_types: &[],
        // Typed per-verb failure (#335): a dirty source, a lost tree, or a busy
        // worktree is DATA an author cases on, not an eval abort. These are
        // `WorktreeError`'s core variants plus two required but not spelled
        // out in the original illustrative ADT, plus three the storage-error
        // lane added — see `tidepool-worktree/src/error.rs` for why each
        // stands alone rather than folding into a neighbour.
        //
        // `error_to_wire` (the ten-arm domain→wire map in
        // `handlers/worktree.rs`) stays HAND-WRITTEN: it maps between two error
        // vocabularies whose variants differ in arity, and several arms carry a
        // decision about which domain failure becomes which wire failure. There
        // is no `DomainMap` slot on `ErrorAdt` because generating that map is
        // not attempted.
        errors: Some(errors()),
        verbs: verbs(),
        helpers: helpers(),
        polymorphism: Polymorphism::None,
        dispatched: true,
    }
}

/// The thirteen supporting declarations, in the order they are emitted.
///
/// This order is the wire contract's outer layer and it reproduces
/// `worktree_effect_def!`'s `type_defs` list exactly — every `data` decl in this
/// order, then every `ToJSON` instance in this order.
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
            // `WorktreeMergeInto` takes a `BranchName` argument (the branch to
            // fold in), so this direction is now exercised — infallible, same
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
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "SourceRef",
                        fields: vec![HsType::Named("GitRef")],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "SourceWorktree",
                        fields: vec![HsType::Named("WorktreeId")],
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
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "AllowDirtySnapshot",
                        fields: vec![],
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
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "InProgressRebase",
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "InProgressCherryPick",
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "InProgressRevert",
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "InProgressBisect",
                        fields: vec![],
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
            name: "MergeOutcome",
            wire_rust: Some("WtMergeOutcome"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "Merged",
                        fields: vec![HsType::Named("GitOid")],
                        doc: &[
                            "The merge landed a new commit — the target's `HEAD` afterward. Always a",
                            "genuine merge commit (`--no-ff`), never a fast-forward.",
                        ],
                    },
                    SumVariant {
                        ctor: "Conflict",
                        fields: vec![HsType::list(HsType::Text)],
                        doc: &[
                            "The merge conflicted. The paths are what git reported unmerged, read",
                            "BEFORE the abort; the target worktree is guaranteed clean by the time",
                            "this is returned — the abort always runs first.",
                        ],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: Some(DomainMap {
                domain_path: "tidepool_worktree::merge::MergeOutcome",
                into_wire: Some(AdapterKind::HandWritten(
                    "`Merged` wraps its `GitOid` through `git_oid_to_wire`; `Conflict` clones \
                     its path `Vec` — both need a conversion beyond a bare variant rename",
                )),
                from_wire: None,
            }),
            doc: &[
                "Haskell `MergeOutcome` — the result of merging one branch into a target",
                "worktree via `mergeBranchInto`. A `git` invocation failure that never",
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
                ctor: "GitFailure",
                fields: vec![field("receipt", "GitFailureReceipt", "WtGitFailureReceipt")],
                doc: "git itself failed; the receipt carries the invocation and its output",
            },
            // The storage-error lane added these three after the block was
            // first written. Each names a failure an author would act on
            // DIFFERENTLY, so each gets its own variant rather than folding
            // into a neighbour: `InvalidRegistryRoot` and `StorageFailure` in
            // particular are not `GitFailure` — no git process runs in either,
            // and spelling them as one would send an operator reading a git
            // receipt that does not exist.
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
            ctor: "WorktreeLookup",
            method: "worktree_lookup",
            args: vec![tree_id_arg()],
            ret: handle,
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
        // The one narrow, deliberate workflow primitive (the
        // worktree-coordination fold — see `tidepool-worktree/src/merge.rs`
        // and this crate's `CLAUDE.md`): merge one branch into a target
        // worktree, typed and classified ONCE, so authored harnesses stop
        // re-deriving conflict-vs-failure over raw `Exec`. It is not a
        // general git-workflow surface — rebase, cherry-pick, and conflict
        // RESOLUTION are still absent by design and still authored policy.
        Verb {
            ctor: "WorktreeMergeInto",
            method: "worktree_merge_into",
            args: vec![
                tree_id_arg(),
                Arg {
                    name: "branch",
                    ty: HsType::Named("BranchName"),
                    rust: RustBinding::Bridged("WtBranchName"),
                },
                Arg {
                    name: "message",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
            ],
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

/// The FIVE representable helpers: four thin one-verb wrappers (three from
/// the original census plus `mergeBranchInto`, added later), plus the one
/// pure projection.
///
/// Ten more live in `haskell/lib/Tidepool/Worktree.hs` as DEFINITIONS. They are
/// excluded rather than smuggled in as strings, and the `extra_imports` row
/// above is what keeps them on the eval surface.
///
/// `worktreeId` is the one relocation candidate that could not
/// go, and the reason is structural: the RepoEvent helpers `commit` and
/// `headChanged` CALL it from inside the same generated `Tidepool.Effects`
/// module, which cannot import `Tidepool.Worktree` (that module imports IT).
/// See [`HelperBody::Projection`], where the whole argument lives.
fn helpers() -> Vec<Helper> {
    vec![
        Helper {
            name: "createWorktree",
            ctor: Some("WorktreeCreate"),
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
            doc: &[
                "Look a retained worktree up by durable id. Survives restart:",
                "resolution reads on-disk registry state, not process memory.",
                "`Left (WorktreeLost i)` when it is registered but gone from disk.",
            ],
            body: HelperBody::Pointfree,
        },
        Helper {
            name: "listWorktrees",
            ctor: Some("WorktreeList"),
            doc: &[
                "Every registered worktree, present or lost. A lost tree is listed",
                "with `present = False` rather than failing the whole listing.",
            ],
            body: HelperBody::NullaryLiftEither,
        },
        Helper {
            name: "worktreeId",
            ctor: None,
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
            name: "mergeBranchInto",
            ctor: Some("WorktreeMergeInto"),
            doc: &[
                "Merge `branch` into the worktree `treeId` names, as `git merge --no-ff`",
                "— never a fast-forward, so a landed merge always carries a genuine merge",
                "commit. On conflict, the conflicting paths are read and the merge is",
                "ABORTED before this returns: the worktree is left clean either way,",
                "success or conflict. `Left (GitFailure r)` is a `git` invocation that",
                "never entered a merge at all (an unknown branch, a locked index) —",
                "distinct from `Right (Conflict paths)`, a merge that genuinely started",
                "and conflicted.",
            ],
            body: HelperBody::Applied(&["treeId", "branch", "message"]),
        },
    ]
}
