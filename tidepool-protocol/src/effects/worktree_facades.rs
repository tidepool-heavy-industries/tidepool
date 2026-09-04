//! Granular Shoal capabilities delegated to the canonical Worktree handler.
//!
//! These effects split the model-facing row along authority decisions. Their
//! Rust handlers translate directly into the existing `WorktreeReq` enum, so
//! Git mechanics, authorization, and typed errors retain one owner.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

const FOREIGN: &[(&str, &str)] = &[
    ("WorktreeError", "crate::generated::worktree::WorktreeError"),
    ("WorktreeSpec", "tidepool_bridge_effects::WtWorktreeSpec"),
    ("DirtyPolicy", "tidepool_bridge_effects::WtDirtyPolicy"),
    (
        "WorktreeHandle",
        "tidepool_bridge_effects::WtWorktreeHandle",
    ),
    ("WorktreeId", "tidepool_bridge_effects::WtWorktreeId"),
    (
        "WorktreeSummary",
        "tidepool_bridge_effects::WtWorktreeSummary",
    ),
    ("BranchName", "tidepool_bridge_effects::WtBranchName"),
    ("GitOid", "tidepool_bridge_effects::WtGitOid"),
    (
        "SubmissionObservation",
        "tidepool_bridge_effects::WtSubmissionObservation",
    ),
    ("MergeRequest", "tidepool_bridge_effects::WtMergeRequest"),
    ("MergeOutcome", "tidepool_bridge_effects::WtMergeOutcome"),
];

#[must_use]
pub fn bound_worktree() -> Effect {
    effect(
        "BoundWorktree",
        "ActorBoundWorktreeHandler",
        "BoundWorktreeReq",
        "bound_worktree_decl",
        vec![
            plain(
                "BoundWorktreeGet",
                "bound_worktree_get",
                vec![],
                result("WorktreeHandle"),
            ),
            plain(
                "BoundWorktreeLookup",
                "bound_worktree_lookup",
                vec![arg(
                    "treeId",
                    "WorktreeId",
                    "tidepool_bridge_effects::WtWorktreeId",
                )],
                result("WorktreeHandle"),
            ),
            plain(
                "BoundWorktreeBranchOf",
                "bound_worktree_branch_of",
                vec![arg(
                    "treeId",
                    "WorktreeId",
                    "tidepool_bridge_effects::WtWorktreeId",
                )],
                result("BranchName"),
            ),
            plain(
                "BoundWorktreeHeadOf",
                "bound_worktree_head_of",
                vec![arg(
                    "treeId",
                    "WorktreeId",
                    "tidepool_bridge_effects::WtWorktreeId",
                )],
                result("GitOid"),
            ),
            plain(
                "BoundWorktreeObserveSubmission",
                "bound_worktree_observe_submission",
                vec![arg(
                    "treeId",
                    "WorktreeId",
                    "tidepool_bridge_effects::WtWorktreeId",
                )],
                result("SubmissionObservation"),
            ),
        ],
    )
}

#[must_use]
pub fn worktree_registry() -> Effect {
    effect(
        "WorktreeRegistry",
        "ActorWorktreeRegistryHandler",
        "WorktreeRegistryReq",
        "worktree_registry_decl",
        vec![
            plain(
                "WorktreeRegistryLookup",
                "worktree_registry_lookup",
                vec![arg(
                    "treeId",
                    "WorktreeId",
                    "tidepool_bridge_effects::WtWorktreeId",
                )],
                result("WorktreeHandle"),
            ),
            plain(
                "WorktreeRegistryList",
                "worktree_registry_list",
                vec![],
                HsType::either(
                    HsType::Named("WorktreeError"),
                    HsType::list(HsType::Named("WorktreeSummary")),
                ),
            ),
            plain(
                "WorktreeRegistryQuery",
                "worktree_registry_query",
                vec![
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
                HsType::either(
                    HsType::Named("WorktreeError"),
                    HsType::list(HsType::Named("WorktreeSummary")),
                ),
            ),
        ],
    )
}

#[must_use]
pub fn worktree_allocation() -> Effect {
    effect(
        "WorktreeAllocation",
        "ActorWorktreeAllocationHandler",
        "WorktreeAllocationReq",
        "worktree_allocation_decl",
        vec![
            plain(
                "WorktreeAllocationCreate",
                "worktree_allocation_create",
                vec![arg(
                    "spec",
                    "WorktreeSpec",
                    "tidepool_bridge_effects::WtWorktreeSpec",
                )],
                result("WorktreeHandle"),
            ),
            plain(
                "WorktreeAllocationCreateForActorPath",
                "worktree_allocation_create_for_actor_path",
                vec![
                    arg(
                        "spec",
                        "WorktreeSpec",
                        "tidepool_bridge_effects::WtWorktreeSpec",
                    ),
                    text_arg("actorPath"),
                ],
                result("WorktreeHandle"),
            ),
            plain(
                "WorktreeAllocationCreateFromBoundForActorPath",
                "worktree_allocation_create_from_bound_for_actor_path",
                vec![
                    arg(
                        "dirtyPolicy",
                        "DirtyPolicy",
                        "tidepool_bridge_effects::WtDirtyPolicy",
                    ),
                    text_arg("actorPath"),
                ],
                result("WorktreeHandle"),
            ),
        ],
    )
}

#[must_use]
pub fn worktree_integration() -> Effect {
    effect(
        "WorktreeIntegration",
        "ActorWorktreeIntegrationHandler",
        "WorktreeIntegrationReq",
        "worktree_integration_decl",
        vec![plain(
            "WorktreeIntegrationTryMerge",
            "worktree_integration_try_merge",
            vec![arg(
                "request",
                "MergeRequest",
                "tidepool_bridge_effects::WtMergeRequest",
            )],
            result("MergeOutcome"),
        )],
    )
}

fn effect(
    name: &'static str,
    handler: &'static str,
    req_enum: &'static str,
    decl_fn: &'static str,
    verbs: Vec<Verb>,
) -> Effect {
    Effect {
        name,
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler,
        handler_module: "worktree",
        req_enum,
        decl_fn,
        description: &["Granular Shoal capability delegated to the canonical Worktree handler."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: FOREIGN,
        errors: None,
        verbs,
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: true,
    }
}

fn plain(ctor: &'static str, method: &'static str, args: Vec<Arg>, ret: HsType) -> Verb {
    Verb {
        ctor,
        method,
        args,
        ret,
        errors: None,
        handling: HandlingClass::OuterDispatch(crate::schema::OuterEffect::Worktree),
        extract: None,
    }
}

fn result(ok: &'static str) -> HsType {
    HsType::either(HsType::Named("WorktreeError"), HsType::Named(ok))
}

fn arg(name: &'static str, ty: &'static str, rust: &'static str) -> Arg {
    Arg {
        name,
        ty: HsType::Named(ty),
        rust: RustBinding::Path(rust),
    }
}

fn text_arg(name: &'static str) -> Arg {
    Arg {
        name,
        ty: HsType::Text,
        rust: RustBinding::Derived,
    }
}
