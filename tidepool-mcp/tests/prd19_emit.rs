//! PRD 19 surface gates: the generated `Tidepool.Effects` must actually carry
//! the authored vocabulary that `Tidepool.Worktree` / `Tidepool.Event`
//! re-export, and must carry the `ToJSON` instances its own `errors` block
//! depends on.
//!
//! Each test here is a ONE-FAILURE-MODE gate and is named for the failure it
//! catches, so a receipt can cite it individually — an aggregate count cannot
//! distinguish "the guard held" from "the guard silently stopped existing".
//!
//! These are cheap string gates on generated source, deliberately: the real
//! typecheck is GHC compiling `Tidepool.Worktree`/`Tidepool.Event` against this
//! module, which is orders of magnitude slower and cannot run in the fast tier.
//! What these catch is the specific regression of a name silently dropping out
//! of the generated surface, which is exactly what a frozen re-export list
//! would then fail on.

fn worktree_and_event_module() -> String {
    let decls = vec![
        tidepool_mcp::console_decl(),
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::event_decl(),
    ];
    tidepool_mcp::effects_module_source(&decls)
}

/// The `errors WorktreeError` block templates a `ToJSON WorktreeError`
/// instance whose arms serialize `DirtySummary`, `InProgressKind`,
/// `GitFailureReceipt` and `WorktreeId`. If any of those four lacks its own
/// `ToJSON`, the GENERATED MODULE DOES NOT COMPILE — and it fails for every
/// row containing the Worktree decl, not just one. That is the regression this
/// gate exists for; it was a real break, found by GHC, before these instances
/// were added.
#[test]
fn worktree_error_payload_types_all_have_tojson_instances() {
    let src = worktree_and_event_module();
    for ty in [
        "WorktreeId",
        "InProgressKind",
        "DirtySummary",
        "GitFailureReceipt",
    ] {
        assert!(
            src.contains(&format!("instance ToJSON {ty} where")),
            "generated Tidepool.Effects is missing `instance ToJSON {ty}`, so the \
             templated `ToJSON WorktreeError` instance cannot compile"
        );
    }
}

/// `Tidepool.Worktree`'s import list is frozen — every name below is one it
/// re-exports, so a name dropping out of the generated module breaks that
/// module's compile rather than degrading gracefully.
#[test]
fn generated_module_carries_the_authored_worktree_surface() {
    let src = worktree_and_event_module();
    for name in [
        "fromCurrentRepository ::",
        "fromRef ::",
        "fromWorktree ::",
        "allowDirtySnapshot ::",
        "createWorktree ::",
        "lookupWorktree ::",
        "listWorktrees ::",
        "worktreeBranch ::",
        "worktreeHead ::",
        "worktreeId ::",
        "renderWorktreeError ::",
        "renderWorktreeId ::",
        "renderGitOid ::",
        "renderBranchName ::",
    ] {
        assert!(
            src.contains(name),
            "generated Tidepool.Effects is missing `{name}`, which Tidepool.Worktree re-exports"
        );
    }
}

/// Same for `Tidepool.Event`, plus the interposition `withHandler` is built
/// from. `pumpEff` recursing on the BODY only is what makes a subscription
/// unable to re-enter its own handler, so its shape is load-bearing, not
/// incidental — see plans/post-restart/worktree-lanes/L4-mechanism.md.
#[test]
fn generated_module_carries_the_authored_event_surface() {
    let src = worktree_and_event_module();
    for name in [
        "commit ::",
        "headChanged ::",
        "(<|>) ::",
        "withHandler ::",
        "pumpEff ::",
        "drainSubscription ::",
        "data Event a = Event",
        "instance Functor Event",
        "data Observed a = Observed",
        "data HeadChangeKind",
        "data RepositoryEvent",
    ] {
        assert!(
            src.contains(name),
            "generated Tidepool.Effects is missing `{name}`, which Tidepool.Event re-exports \
             or withHandler is built from"
        );
    }
}

/// `withHandler` interposes on the body's freer structure, which needs `Eff`'s
/// own constructors and queue operations. `Control.Monad.Freer` re-exports the
/// TYPE but not `Val`/`E`, `qApp`, or `tsingleton`, so without this import the
/// pump cannot be written at all.
#[test]
fn generated_module_imports_freer_internal_for_the_pump() {
    let src = worktree_and_event_module();
    assert!(
        src.contains("import Control.Monad.Freer.Internal (Eff(..), qApp, tsingleton)"),
        "generated Tidepool.Effects must import Eff's constructors for pumpEff's interposition"
    );
}

/// `worktreeHead` is a FRESH read and must be effectful. If it ever degraded to
/// a pure accessor it would be returning recorded state — the seed commit —
/// which looks like it works while leaving open exactly the cross-cycle gap it
/// exists to close. `worktreeId` is the opposite case and must stay pure.
#[test]
fn worktree_head_is_effectful_and_worktree_id_is_pure() {
    let src = worktree_and_event_module();
    assert!(
        src.contains("worktreeHead :: WorktreeHandle -> M GitOid"),
        "worktreeHead must be a fresh, effectful read — not a pure accessor over recorded state"
    );
    assert!(
        src.contains("worktreeId :: WorktreeHandle -> WorktreeId"),
        "worktreeId must stay pure — the handle already carries its receipt"
    );
}
