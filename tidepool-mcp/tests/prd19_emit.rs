//! PRD 19 surface gates: the generated `Tidepool.Effects` must actually carry
//! the authored vocabulary that `Tidepool.Worktree` / `Tidepool.Event`
//! re-export, and must carry the `ToJSON` instances its own `errors` block
//! depends on.
//!
//! `Tidepool.Worktree` re-exports FOUR of its fourteen names now — the three
//! thin one-verb wrappers plus the pure projection `worktreeId`, the helper
//! shapes the effect contract can represent.
//! The other ten are DEFINITIONS in that module (scaffold doc §11.9), so the
//! gates covering them read the authored source rather than the generated one,
//! and the generated module is gated on NOT redefining them.
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

/// Emit the generated module for `scripts/prd19-alternative-gates.sh`, which
/// needs the real source to run its three GHC gates against. `#[ignore]`d so it
/// never runs as part of an ordinary test pass — it is a fixture producer, not
/// a gate, and it does nothing without `PRD19_EMIT` set.
#[test]
#[ignore = "fixture producer for scripts/prd19-alternative-gates.sh"]
fn emit_for_gates() {
    let path = std::env::var("PRD19_EMIT").expect("PRD19_EMIT must name the output path");
    let p = std::path::Path::new(&path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    std::fs::write(p, worktree_and_event_module()).expect("write generated module");
}

/// The `(<|>)` COLLISION IS OPEN, RELOCATED — this gate pins the honest
/// current state after PRD 22 lane 4's flip moved `(<|>)` OUT of the
/// generated `Tidepool.Effects` module entirely (it is now a DEFINITION in
/// `haskell/lib/Tidepool/Event.hs`, not schema-representable — see that
/// module's own doc). The collision this gate originally tracked —
/// `Tidepool.Prelude`'s `Alternative` `(<|>)` vs Event's merge `(<|>)`, both
/// reachable unqualified once RepoEvent is in a row — is UNCHANGED in kind,
/// only in WHERE it lives: `Tidepool.Event.hs` hides Prelude's `(<|>)` for
/// its OWN internal resolution (needed just to define `(<|>)` without an
/// ambiguous-occurrence error at its own export list), but an EVAL importing
/// both `Tidepool.Prelude` (auto) and `Tidepool.Event` (via `extra_imports`)
/// still sees both unqualified — the fix is still ROUTED to the
/// AUTHOR-FACING imports (`preamble.rs`'s `eval_import_lines`), not landed
/// here, for the same reason the original gate gave.
///
/// This gate therefore asserts: the generated module no longer defines its
/// own `(<|>)` (moved out), `Tidepool.Event.hs` does (where the collision
/// now lives), and the eval preamble's own `Tidepool.Prelude` import is
/// UNCHANGED (still not hiding `(<|>)`) — so that whoever lands the routed
/// fix sees this test move and knows where to retarget it.
#[test]
fn the_alternative_collision_is_open_and_the_generated_module_is_unchanged() {
    let src = worktree_and_event_module();
    assert!(
        !src.contains("(<|>) ::"),
        "generated Tidepool.Effects no longer defines (<|>) — it relocated to \
         Tidepool.Event.hs (PRD 22 lane 4)"
    );
    assert!(
        authored_event_module().contains("(<|>) :: Event a -> Event a -> Event a"),
        "Tidepool.Event.hs must define (<|>) — this is where the open collision now lives"
    );
    let decls = vec![tidepool_mcp::console_decl(), tidepool_mcp::event_decl()];
    assert!(
        tidepool_mcp::build_preamble(&decls, false)
            .contains("import Tidepool.Prelude hiding (error)\n"),
        "the AUTHOR-FACING Prelude import must stay byte-identical to its pre-PRD-19 form — \
         the routed fix has not landed"
    );
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

/// The four helpers `Tidepool.Worktree` still RE-EXPORTS are frozen — three
/// thin wrappers over one verb plus one pure projection, so each is
/// schema-representable and stays in the contract. A name dropping out of the
/// generated module breaks that module's compile rather than degrading
/// gracefully.
#[test]
fn generated_module_carries_the_authored_worktree_surface() {
    let src = worktree_and_event_module();
    for name in [
        "createWorktree ::",
        "lookupWorktree ::",
        "listWorktrees ::",
        "worktreeId ::",
    ] {
        assert!(
            src.contains(name),
            "generated Tidepool.Effects is missing `{name}`, which Tidepool.Worktree re-exports"
        );
    }
}

/// The other side of the same contract: the ten names that are NOT
/// schema-representable (scaffold doc §11.9) are DEFINED in
/// `haskell/lib/Tidepool/Worktree.hs` and must NOT also be emitted here. Two
/// definitions of `fromCurrentRepository` in one row is a duplicate-binding
/// error the moment that module is imported, and the import is exactly what
/// `extra_imports` now arranges — so this gate is what keeps the relocation
/// from silently becoming a collision.
#[test]
fn generated_module_does_not_redefine_the_relocated_worktree_helpers() {
    let src = worktree_and_event_module();
    for name in [
        "fromCurrentRepository ::",
        "fromRef ::",
        "fromWorktree ::",
        "allowDirtySnapshot ::",
        "worktreeBranch ::",
        "worktreeHead ::",
        "renderWorktreeError ::",
        "renderWorktreeId ::",
        "renderGitOid ::",
        "renderBranchName ::",
    ] {
        assert!(
            !src.contains(name),
            "generated Tidepool.Effects still defines `{name}`, which now lives in \
             haskell/lib/Tidepool/Worktree.hs — two definitions in one row collide"
        );
        assert!(
            authored_worktree_module().contains(name),
            "haskell/lib/Tidepool/Worktree.hs no longer defines `{name}`, and the \
             generated module no longer does either — the name has no home"
        );
    }
}

/// The relocation is invisible to an eval author only because the Worktree
/// decl carries the companion import. Without this row a row containing
/// Worktree would see ten fewer names than it did before.
///
/// The import lands in the AUTHOR's preamble, not inside `Tidepool.Effects` —
/// which is the only direction that can work, since `Tidepool.Worktree` imports
/// `Tidepool.Effects` and the reverse would be a module cycle. Both author-
/// facing planes fold `EffectDecl::extra_imports` the same way, so both are
/// asserted here.
#[test]
fn worktree_decl_imports_the_module_that_defines_the_relocated_helpers() {
    let decls = vec![tidepool_mcp::console_decl(), tidepool_mcp::worktree_decl()];
    assert_eq!(
        tidepool_mcp::worktree_decl().extra_imports.to_vec(),
        vec!["import Tidepool.Worktree"]
    );
    assert!(
        tidepool_mcp::build_preamble(&decls, false).contains("import Tidepool.Worktree\n"),
        "an eval whose row carries Worktree must import Tidepool.Worktree"
    );
    assert!(
        tidepool_mcp::session_decl_module_env(&decls, false)
            .imports
            .iter()
            .any(|i| i == "import Tidepool.Worktree"),
        "a session DECL whose row carries Worktree must import Tidepool.Worktree"
    );
}

/// The FOUR helpers `tidepool-protocol`'s Event schema represents (PRD 22
/// lane 4) — thin wrappers over the capability-mailbox trio plus the
/// blocking-wait primitive `nextEvent`/`awaitFirst` build on — plus the
/// representable TYPE declarations (`Watch`/`HeadChangeKind`/
/// `RepositoryEvent`/… stay generated; only `Event`/`Observed` and the
/// `Functor` instance are non-representable, see the test below).
#[test]
fn generated_module_carries_the_authored_event_surface() {
    let src = worktree_and_event_module();
    for name in [
        "awaitSubscriptionRaw ::",
        "mailboxNew ::",
        "mailboxSend ::",
        "mailboxDrop ::",
        "data HeadChangeKind",
        "data RepositoryEvent",
        "data Watch",
    ] {
        assert!(
            src.contains(name),
            "generated Tidepool.Effects is missing `{name}`, which the Event schema represents"
        );
    }
}

/// The other side of the same contract: `commit`/`headChanged`/`(<|>)`/
/// `pumpEff`/`drainSubscription`/`withHandler`/`eventIdOf`/`firstMatch`/
/// `nextEvent`/`awaitFirst`/`after`/`projectTick`/`mailbox`/`projectMailbox`/
/// `asyncDone`/`projectAsyncDone` (eighteen names) plus `Event`/`Observed`
/// (genuinely polymorphic types with no schema vocabulary) are DEFINED in
/// `haskell/lib/Tidepool/Event.hs` and must NOT also be emitted here — same
/// discipline as Worktree's relocated-helpers gate, extended to type
/// declarations for the first time (PRD 22 lane 4).
#[test]
fn generated_module_does_not_redefine_the_relocated_event_helpers() {
    let src = worktree_and_event_module();
    for name in [
        "commit ::",
        "headChanged ::",
        "(<|>) ::",
        "pumpEff ::",
        "drainSubscription ::",
        "withHandler ::",
        "eventIdOf ::",
        "firstMatch ::",
        "nextEvent ::",
        "awaitFirst ::",
        "after ::",
        "projectTick ::",
        "mailbox ::",
        "projectMailbox ::",
        "asyncDone ::",
        "projectAsyncDone ::",
        "data Event a = Event",
        "instance Functor Event",
        "data Observed a = Observed",
    ] {
        assert!(
            !src.contains(name),
            "generated Tidepool.Effects still defines `{name}`, which now lives in \
             haskell/lib/Tidepool/Event.hs — two definitions in one row collide"
        );
        assert!(
            authored_event_module().contains(name),
            "haskell/lib/Tidepool/Event.hs no longer defines `{name}`, and the generated \
             module no longer does either — the name has no home"
        );
    }
}

/// The relocation is invisible to an eval author only because the RepoEvent
/// decl carries the companion import — same mechanism as Worktree's, on the
/// other side of the module cycle (`Tidepool.Event` imports
/// `Tidepool.Effects`, never the reverse).
#[test]
fn event_decl_imports_the_module_that_defines_the_relocated_helpers() {
    let decls = vec![tidepool_mcp::console_decl(), tidepool_mcp::event_decl()];
    assert_eq!(
        tidepool_mcp::event_decl().extra_imports.to_vec(),
        vec!["import Tidepool.Event"]
    );
    assert!(
        tidepool_mcp::build_preamble(&decls, false).contains("import Tidepool.Event\n"),
        "an eval whose row carries RepoEvent must import Tidepool.Event"
    );
    assert!(
        tidepool_mcp::session_decl_module_env(&decls, false)
            .imports
            .iter()
            .any(|i| i == "import Tidepool.Event"),
        "a session DECL whose row carries RepoEvent must import Tidepool.Event"
    );
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

/// The authored `Tidepool.Worktree` source, which now DEFINES the ten
/// non-representable helpers rather than re-exporting them (scaffold doc
/// §11.9). Read from the tree because that is where the signatures live; the
/// gates below are the same one-failure-mode string gates, following their
/// subject.
fn authored_worktree_module() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-mcp lives one level under the workspace root")
        .join("haskell/lib/Tidepool/Worktree.hs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The authored `Tidepool.Event` source, which now DEFINES eighteen
/// non-representable helpers plus `Event`/`Observed`/the `Functor` instance
/// (PRD 22 lane 4) rather than being a pure re-export module.
fn authored_event_module() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-mcp lives one level under the workspace root")
        .join("haskell/lib/Tidepool/Event.hs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// `worktreeHead` is a FRESH read and must be effectful. If it ever degraded to
/// a pure accessor it would be returning recorded state — the seed commit —
/// which looks like it works while leaving open exactly the cross-cycle gap it
/// exists to close. `worktreeId` is the opposite case and must stay pure.
#[test]
fn worktree_head_is_effectful_and_worktree_id_is_pure() {
    assert!(
        authored_worktree_module().contains("worktreeHead :: WorktreeHandle -> M GitOid"),
        "worktreeHead must be a fresh, effectful read — not a pure accessor over recorded state"
    );
    assert!(
        worktree_and_event_module().contains("worktreeId :: WorktreeHandle -> WorktreeId"),
        "worktreeId must stay pure — the handle already carries its receipt"
    );
}
