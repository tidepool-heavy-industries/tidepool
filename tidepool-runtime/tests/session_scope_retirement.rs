//! SCOPE RETIREMENT ACCOUNTING — retiring a scope releases its mounts' GC
//! roots FOR REAL, witnessed by the fourth counted root class rather than
//! asserted about.
//!
//! The claim the mount seam alone could not make: a mounted root lived until
//! the session machine dropped, because `release_handle` deliberately does
//! not deregister the underlying persistent root and nothing downstream ever
//! gave that ownership back. This suite pins the closure of that leak, and
//! pins it as COUNTS:
//!
//! - `persistent_roots_count()` (class 4, the GC root ledger) drops by exactly
//!   the receipt's `roots_released` — never "some" roots, never zero-with-a-
//!   cheerful-receipt;
//! - classes 1 (`stowed_roots_count() == parked_count()`) and 2
//!   (`value_handle_count()`) are UNCHANGED by a retirement, including with a
//!   live handle outstanding over an unrelated root;
//! - class 3 (`scope_binding_count`) returns to 0 for the retired scope and to
//!   its pre-scope baseline at ROOT;
//! - the SOLE-OWNERSHIP RULE: a slot a surviving scope's entry also holds is
//!   NOT deregistered — the escaped-closure safety property;
//! - retiring an already-retired scope is a no-op.
//!
//! # Tier: GHC-free, but not quick-tier by location
//!
//! Nothing here runs a GHC extract: the session machine is built from a
//! hand-written `CoreExpr` (`C1 n`) and each tenured, persistent-rooted value
//! comes from `JitEffectMachine::run_pure_and_bind` — the SAME tenure +
//! `register_persistent_root` path a real bind takes, just reached without the
//! Haskell front end. `TIDEPOOL_EXTRACT` is not required.
//!
//! It still needs `--ignore-default-filter -p tidepool-runtime` to run:
//! `.config/nextest.toml` skips `package(tidepool-runtime)` WHOLESALE, so
//! crate membership (not this file's cost) is what puts it outside the quick
//! tier.
//!
//! # Why the value plane is driven through `PersistentSession`
//!
//! `PersistentSession` is where both halves live — the binding table and the
//! machine — so it is where `retire_scope` is implemented and where the
//! accounting is observable. `ResidentSession` re-exports the same surface as
//! thin delegations, and exposes no machine accessor to mint a `ValueHandle`
//! from a test; the delegation shape is covered separately at the bottom.

use tidepool_codegen::binding_table::{BindingEntry, BoundValue};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::old_space::RootSlot;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::{DataConId, Literal, VarId};
use tidepool_repr::{
    BindingName, CoreExpr, CoreFrame, DataConTable, Generation, SessionModule, SessionVarId,
    TreeBuilder,
};
use tidepool_runtime::session::{PersistentSession, SessionError};

/// `C1 :: Int -> T`, the one constructor every session fixture here needs.
const C1: DataConId = DataConId(1);

/// The `Ask` union tag; nothing in this suite suspends.
const ASK_TAG: u64 = 0;

fn table_with_c1() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table
}

/// `C1 n` — a plain bindable value fragment.
fn value_fragment(n: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![lit],
    });
    b.build()
}

/// A bootstrapped, value-plane-only session (no decl plane).
fn session() -> PersistentSession {
    let table = table_with_c1();
    let mut core = PersistentSession::new(None, ASK_TAG, Vec::new(), 1 << 16);
    core.bootstrap_if_needed(&value_fragment(0), &table)
        .expect("compile_session");
    core.seed_session_table(table);
    core
}

/// Tenure `C1 n` into the session heap and return its registered persistent
/// root — the same `OldSpace::tenure` → `register_persistent_root` path an
/// ordinary `x <- e` bind takes.
fn tenure(core: &mut PersistentSession, label: &str, n: i64) -> RootSlot {
    let table = table_with_c1();
    let machine = core.machine_mut().expect("bootstrapped");
    let frag = machine
        .add_function(label, &value_fragment(n), &table, &ExternalEnv::new())
        .expect("add_function");
    machine.run_pure_and_bind(frag).expect("tenure")
}

/// The mount transit, spelled out: a handle is minted over a tenured root,
/// then its slot is read and the handle RELEASED — ownership moving from the
/// handle registry (class 2) to the value plane (class 3). This is exactly
/// what `ResidentSession::mount_handle_in` does; done by hand here because a
/// test cannot reach the machine through `ResidentSession`.
fn mount(core: &mut PersistentSession, scope: ScopeId, name: &str, raw: u64, slot: RootSlot) {
    let machine = core.machine_mut().expect("bootstrapped");
    let handle = machine.mint_handle_from_root(slot, tidepool_codegen::suspension::RealmId(0));
    let mounted = machine.handle_slot(handle).expect("handle is live");
    assert!(machine.release_handle(handle), "handle released once");
    core.bind_in(scope, entry(name, raw, mounted))
        .expect("scope is live");
}

fn entry(name: &str, raw: u64, slot: RootSlot) -> BindingEntry {
    BindingEntry {
        name: BindingName(name.to_string()),
        id: SessionVarId::from_var(VarId((0xFE << 56) | raw)),
        module: SessionModule::val(Generation(raw)),
        value: BoundValue::Tier1Closure(slot),
        type_display: Some("Int -> Int".to_string()),
        defining_expr: None,
        // Overwritten by `bind_in`; ROOT is the honest default for a literal.
        scope: ScopeId::ROOT,
    }
}

/// Classes 1 and 2, read together — a scope retirement must leave BOTH exactly
/// where it found them.
fn classes_1_and_2(core: &PersistentSession) -> (usize, usize, usize) {
    let m = core.machine().expect("bootstrapped");
    (
        m.stowed_roots_count(),
        m.parked_count(),
        m.value_handle_count(),
    )
}

// ---------------------------------------------------------------------------
// (a) + (b) + (c): the ledger drops by exactly what the receipt reports, the
// other classes do not move, and class 3 returns to baseline.
// ---------------------------------------------------------------------------

#[test]
fn retiring_a_scope_releases_exactly_the_roots_its_receipt_reports() {
    let mut core = session();

    // A ROOT-scope mount (the flat session's), plus a live handle over its own
    // separate root: class 2 is NON-ZERO across the retirement, so "unchanged"
    // is a real observation rather than 0 == 0.
    let root_slot = tenure(&mut core, "root_val", 7);
    mount(&mut core, ScopeId::ROOT, "keep", 1, root_slot);
    let handle_slot = tenure(&mut core, "handle_val", 8);
    let outstanding = core
        .machine_mut()
        .expect("bootstrapped")
        .mint_handle_from_root(handle_slot, tidepool_codegen::suspension::RealmId(0));

    let baseline_roots = core.persistent_roots_count();
    let baseline_root_frame = core.scope_binding_count(ScopeId::ROOT);
    let baseline_classes = classes_1_and_2(&core);
    assert_eq!(baseline_root_frame, 1, "one flat-session mount");
    assert_eq!(baseline_classes.2, 1, "one outstanding handle");

    // A child scope with two mounts, and a grandchild with one — retirement
    // must walk the whole subtree.
    let child = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");
    let grandchild = core.mint_scope(child).expect("child is live");
    for (scope, name, raw, n) in [
        (child, "a", 2, 11),
        (child, "b", 3, 12),
        (grandchild, "c", 4, 13),
    ] {
        let slot = tenure(&mut core, name, n);
        mount(&mut core, scope, name, raw, slot);
    }

    assert_eq!(core.scope_binding_count(child), 2);
    assert_eq!(core.scope_binding_count(grandchild), 1);
    assert_eq!(
        core.persistent_roots_count(),
        baseline_roots + 3,
        "three scoped mounts, three new GC roots"
    );
    assert_eq!(
        classes_1_and_2(&core).2,
        1,
        "mounting transfers OUT of the handle registry; the outstanding \
         handle is the only member"
    );
    // The child reads the parent's mount, and the parent gained nothing.
    assert!(core.resolve_in(child, "keep").is_some());
    assert!(core.resolve_in(ScopeId::ROOT, "a").is_none());
    assert_eq!(core.scope_binding_count(ScopeId::ROOT), baseline_root_frame);

    let before = core.persistent_roots_count();
    let receipt = core.retire_scope(child);

    // (a) the ledger drops by exactly what the receipt claims.
    assert_eq!(receipt.scopes_retired, 2, "child + grandchild");
    assert_eq!(receipt.bindings_retired, 3);
    assert_eq!(receipt.roots_released, 3);
    assert_eq!(
        before - core.persistent_roots_count(),
        receipt.roots_released,
        "class 4 is the WITNESS: the ledger moved by exactly the reported \
         release"
    );
    assert_eq!(core.persistent_roots_count(), baseline_roots);

    // (b) classes 1 and 2 are untouched.
    assert_eq!(
        classes_1_and_2(&core),
        baseline_classes,
        "a scope retirement touches neither parked continuations nor the \
         handle registry"
    );
    assert!(
        core.machine_mut()
            .expect("bootstrapped")
            .release_handle(outstanding),
        "the outstanding handle survived the retirement intact"
    );

    // (c) class 3 back to zero for the scope, and to baseline at ROOT.
    assert_eq!(core.scope_binding_count(child), 0);
    assert_eq!(core.scope_binding_count(grandchild), 0);
    assert_eq!(core.scope_binding_count(ScopeId::ROOT), baseline_root_frame);
    assert_eq!(core.bindings().iter_current().count(), baseline_root_frame);
    assert!(
        core.bindings().resolve("keep").is_some(),
        "the flat mount is untouched"
    );
    // The retired ids are dead, so a stale reference resolves nothing rather
    // than falling back to ROOT.
    assert!(core.resolve_in(child, "keep").is_none());
    assert!(core.resolve_in(child, "a").is_none());
}

// ---------------------------------------------------------------------------
// (d) the sole-ownership rule — the escaped-closure safety property.
// ---------------------------------------------------------------------------

#[test]
fn a_slot_a_surviving_scope_also_holds_is_not_deregistered() {
    let mut core = session();

    // ONE tenured value, reachable under two names: the escapee is mounted
    // into a PARENT-scope binding while the producing child also names it.
    let escapee = tenure(&mut core, "escapee", 42);
    mount(&mut core, ScopeId::ROOT, "escaped", 1, escapee);

    let child = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");
    core.bind_in(child, entry("local", 2, escapee))
        .expect("child is live");
    // ...plus a mount the child SOLELY owns, so the receipt distinguishes.
    let owned = tenure(&mut core, "owned", 43);
    mount(&mut core, child, "owned", 3, owned);

    let before = core.persistent_roots_count();
    let receipt = core.retire_scope(child);

    assert_eq!(receipt.bindings_retired, 2, "both names retire");
    assert_eq!(
        receipt.roots_released, 1,
        "only the SOLELY-owned root is released; the aliased one stays \
         registered because a surviving scope still holds it"
    );
    assert_eq!(before - core.persistent_roots_count(), 1);

    // The escapee is still a live, rooted, resolvable binding at ROOT — which
    // is what keeps its captured child-heap subgraph traced.
    let survivor = core
        .bindings()
        .resolve("escaped")
        .expect("still bound at ROOT");
    assert!(std::ptr::eq(survivor.value.root().addr(), escapee.addr()));
    assert_eq!(core.scope_binding_count(child), 0);

    // And retiring the surviving owner's scope is not possible (ROOT never
    // retires), so the escapee outlives every child — the session-lifetime
    // guarantee the flat plane always had.
    assert_eq!(core.retire_scope(ScopeId::ROOT).scopes_retired, 0);
    assert!(core.bindings().resolve("escaped").is_some());
    assert_eq!(core.persistent_roots_count(), before - 1);
}

/// Two names in the SAME retiring frame over ONE root: the alias check cannot
/// see it (both are drained before either is examined), so exactly-once is
/// carried by the released-address set instead. One release, one ledger step.
#[test]
fn two_names_in_one_frame_over_one_root_release_it_exactly_once() {
    let mut core = session();
    let child = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");
    let shared = tenure(&mut core, "shared", 9);
    core.bind_in(child, entry("first", 1, shared))
        .expect("child is live");
    core.bind_in(child, entry("second", 2, shared))
        .expect("child is live");

    let before = core.persistent_roots_count();
    let receipt = core.retire_scope(child);
    assert_eq!(receipt.bindings_retired, 2);
    assert_eq!(receipt.roots_released, 1, "one root, one release");
    assert_eq!(before - core.persistent_roots_count(), 1);
}

// ---------------------------------------------------------------------------
// (e) idempotence, and the untouched flat session.
// ---------------------------------------------------------------------------

#[test]
fn retiring_an_already_retired_scope_is_a_no_op() {
    let mut core = session();
    let child = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");
    let slot = tenure(&mut core, "v", 5);
    mount(&mut core, child, "v", 1, slot);

    let first = core.retire_scope(child);
    assert_eq!(first.roots_released, 1);
    let after = core.persistent_roots_count();

    let second = core.retire_scope(child);
    assert_eq!(second.scopes_retired, 0);
    assert_eq!(second.bindings_retired, 0);
    assert_eq!(
        second.roots_released, 0,
        "no double deregistration, and no false receipt for one"
    );
    assert_eq!(core.persistent_roots_count(), after);
}

#[test]
fn a_retired_scopes_sibling_and_the_flat_session_are_untouched() {
    let mut core = session();
    let flat = tenure(&mut core, "flat", 1);
    mount(&mut core, ScopeId::ROOT, "x", 1, flat);
    let left = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");
    let right = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");

    // Both siblings bind the SAME name — disjoint frames, no collision.
    let l_slot = tenure(&mut core, "l", 2);
    let r_slot = tenure(&mut core, "r", 3);
    mount(&mut core, left, "helper", 2, l_slot);
    mount(&mut core, right, "helper", 3, r_slot);
    assert!(!std::ptr::eq(
        core.resolve_in(left, "helper")
            .expect("l")
            .value
            .root()
            .addr(),
        core.resolve_in(right, "helper")
            .expect("r")
            .value
            .root()
            .addr(),
    ));
    assert!(
        core.bindings().resolve("helper").is_none(),
        "the parent gained neither"
    );

    let before = core.persistent_roots_count();
    let receipt = core.retire_scope(left);
    assert_eq!(receipt.roots_released, 1);
    assert_eq!(core.persistent_roots_count(), before - 1);

    assert!(core.resolve_in(right, "helper").is_some(), "sibling intact");
    assert_eq!(core.scope_binding_count(right), 1);
    assert_eq!(core.current_val_modules(), vec!["Tidepool.Session.Val.G1"]);
    assert_eq!(
        core.current_val_modules_in(right),
        vec!["Tidepool.Session.Val.G1", "Tidepool.Session.Val.G3"],
        "the sibling sees its own mount plus the inherited flat one"
    );
}

// ---------------------------------------------------------------------------
// (f) the liveness precondition — a dead scope can never receive a mount.
// ---------------------------------------------------------------------------

/// `bind_in` on a dead scope (never minted, or already retired) must reject
/// with a typed [`SessionError::DeadScope`] rather than writing a binding no
/// lookup chain can ever see and no `retire_scope` can ever drain — the
/// 2026-08-19 review's HIGH finding, at the layer `PersistentSession` owns
/// the `ScopeTree` from. Covers both shapes of "dead": an id that was live
/// and is now retired, and one that was never minted at all.
#[test]
fn bind_in_rejects_a_dead_scope_and_touches_nothing() {
    let mut core = session();
    let child = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");
    core.retire_scope(child);

    let baseline_roots = core.persistent_roots_count();
    let baseline_root_frame = core.scope_binding_count(ScopeId::ROOT);

    let slot = tenure(&mut core, "orphan", 99);
    let result = core.bind_in(child, entry("orphan", 1, slot));
    assert!(
        matches!(result, Err(SessionError::DeadScope(s)) if s == child),
        "expected a typed DeadScope error for a retired scope, got {result:?}"
    );

    let never_minted = ScopeId(999_999);
    let result = core.bind_in(never_minted, entry("orphan2", 2, slot));
    assert!(
        matches!(result, Err(SessionError::DeadScope(s)) if s == never_minted),
        "expected a typed DeadScope error for a never-minted scope, got {result:?}"
    );

    // Rejected binds tenured a root (via `tenure`, the same path `run_bind`
    // takes) but never MOUNTED it — the ledger only reflects the tenure
    // itself, and nothing is resolvable under either dead scope.
    assert_eq!(
        core.persistent_roots_count(),
        baseline_roots + 1,
        "the tenure itself still registers a root (an ordinary bind's own \
         completion) — bind_in's rejection is about the BINDING, not the \
         tenure that already happened before it was called"
    );
    assert_eq!(core.scope_binding_count(ScopeId::ROOT), baseline_root_frame);
    assert!(core.resolve_in(child, "orphan").is_none());
    assert!(core.bindings().resolve("orphan").is_none());
}

// ---------------------------------------------------------------------------
// The `ResidentSession` delegation surface: every no-arg API means ROOT.
// ---------------------------------------------------------------------------

mod resident_delegation {
    use super::*;
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_effect::error::EffectError;
    use tidepool_effect::Response;
    use tidepool_runtime::session::ResidentSession;

    #[derive(Clone, Default)]
    struct Sink;
    impl tidepool_runtime::session::OutputSink for Sink {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }
        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }

    struct NoDispatch;
    impl DispatchEffect<Sink> for NoDispatch {
        fn dispatch(
            &mut self,
            tag: u64,
            _request: &tidepool_eval::value::Value,
            _cx: &EffectContext<'_, Sink>,
        ) -> Result<Response, EffectError> {
            panic!("nothing in this suite dispatches (tag {tag})");
        }
    }

    /// A bootstrapped resident session over the same hand-built seed.
    fn resident() -> ResidentSession<NoDispatch, Sink> {
        ResidentSession::bootstrap(
            &value_fragment(0),
            table_with_c1(),
            NoDispatch,
            ASK_TAG,
            Vec::new(),
            Sink,
            Vec::new(),
            1 << 16,
            None,
        )
        .expect("bootstrap")
    }

    #[test]
    fn the_no_arg_surface_means_root_and_an_empty_scope_retires_to_zero() {
        let mut s = resident();
        assert_eq!(s.persistent_roots_count(), 0, "no binds yet");
        assert_eq!(s.binding_names(), s.binding_names_in(ScopeId::ROOT));
        assert_eq!(s.scope_binding_count(ScopeId::ROOT), 0);

        let child = s.mint_scope(ScopeId::ROOT).expect("ROOT is live");
        assert_eq!(s.scope_binding_count(child), 0);
        assert_eq!(
            s.binding_names_in(child),
            s.binding_names(),
            "an empty child frame sees exactly what ROOT has"
        );
        assert!(s.current_binding_in(child, "nope").is_none());

        let receipt = s.retire_scope(child);
        assert_eq!(receipt.scopes_retired, 1);
        assert_eq!(receipt.bindings_retired, 0);
        assert_eq!(receipt.roots_released, 0);
        assert_eq!(s.persistent_roots_count(), 0);

        // ROOT never retires, and a re-retire of a dead scope is a no-op.
        assert_eq!(s.retire_scope(ScopeId::ROOT), Default::default());
        assert_eq!(s.retire_scope(child), Default::default());
        assert!(s.mint_scope(child).is_none(), "a dead scope cannot parent");
    }
}
