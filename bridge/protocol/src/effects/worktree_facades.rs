//! Granular Exomonad capabilities delegated to the canonical Worktree handler.
//!
//! These effects split the model-facing row along authority decisions. Their
//! Rust handlers translate directly into the existing `WorktreeReq` enum, so
//! Git mechanics, authorization, and typed errors retain one owner.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

const FOREIGN: &[crate::schema::ExternalType] = &[
    crate::schema::ExternalType {
        haskell_name: "WorkspaceHandle",
        rust_wire: "tidepool_bridge_effects::WtWorkspaceHandle",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "WorktreeError",
        rust_wire: "crate::generated::worktree::WorktreeError",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "WorktreeSpec",
        rust_wire: "tidepool_bridge_effects::WtWorktreeSpec",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "DirtyPolicy",
        rust_wire: "tidepool_bridge_effects::WtDirtyPolicy",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "WorktreeHandle",
        rust_wire: "tidepool_bridge_effects::WtWorktreeHandle",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "WorktreeId",
        rust_wire: "tidepool_bridge_effects::WtWorktreeId",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "WorktreeSummary",
        rust_wire: "tidepool_bridge_effects::WtWorktreeSummary",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "BranchName",
        rust_wire: "tidepool_bridge_effects::WtBranchName",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "GitOid",
        rust_wire: "tidepool_bridge_effects::WtGitOid",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "SubmissionObservation",
        rust_wire: "tidepool_bridge_effects::WtSubmissionObservation",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "MergeRequest",
        rust_wire: "tidepool_bridge_effects::WtMergeRequest",
        core_module: None,
    },
    crate::schema::ExternalType {
        haskell_name: "MergeOutcome",
        rust_wire: "tidepool_bridge_effects::WtMergeOutcome",
        core_module: None,
    },
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
                "BoundWorkspaceGet",
                "bound_workspace_get",
                vec![],
                result("WorkspaceHandle"),
            ),
            plain(
                "BoundWorktreeGet",
                "bound_worktree_get",
                vec![],
                result("WorktreeHandle"),
            ),
            plain(
                "BoundWorktreeLookup",
                "bound_worktree_lookup",
                vec![arg("treeId", "WorktreeId")],
                result("WorktreeHandle"),
            ),
            plain(
                "BoundWorktreeBranchOf",
                "bound_worktree_branch_of",
                vec![arg("treeId", "WorktreeId")],
                result("BranchName"),
            ),
            plain(
                "BoundWorktreeHeadOf",
                "bound_worktree_head_of",
                vec![arg("treeId", "WorktreeId")],
                result("GitOid"),
            ),
            plain(
                "BoundWorktreeObserveSubmission",
                "bound_worktree_observe_submission",
                vec![arg("treeId", "WorktreeId")],
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
                vec![arg("treeId", "WorktreeId")],
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
        vec![plain(
            "WorktreeAllocationCreate",
            "worktree_allocation_create",
            vec![arg("spec", "WorktreeSpec")],
            result("WorktreeHandle"),
        )],
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
            vec![arg("request", "MergeRequest")],
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
        description: &["Granular Exomonad capability delegated to the canonical Worktree handler."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: Vec::new(),
        external_types: FOREIGN,
        errors: None,
        verbs,
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        generated_handler: true,
        handler_execution: crate::schema::HandlerExecution::BlockingPrepared,
        caller_principal: false,
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
    }
}

fn result(ok: &'static str) -> HsType {
    HsType::either(HsType::Named("WorktreeError"), HsType::Named(ok))
}

fn arg(name: &'static str, ty: &'static str) -> Arg {
    Arg {
        name,
        ty: HsType::Named(ty),
        rust: RustBinding::External,
    }
}
