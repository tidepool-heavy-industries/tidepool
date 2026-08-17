//! Cross-realm isolation: realm B's bindings must never be seeded into realm
//! A's compiled fragment. That property holds today only as a corollary of
//! fresh-id minting plus `BindingTable::seed_external_env` being narrowed to
//! the fragment's referenced VarIds — `binding_table.rs`'s
//! `seed_external_env_*` tests pin "narrowing the seed doesn't narrow
//! GC-root retention," not this isolation property, so this test states it
//! directly: two independent scopes sharing ONE `BindingTable`, colliding on
//! display name (`x`, `tmp`), each with freshly-minted `SessionVarId`s.
//! Asserted on ids, never on counts (a count assertion passes vacuously if
//! both scopes' ids happen to be seeded and one is also missing), and in
//! BOTH directions (the leak is not symmetric under all plausible
//! regressions).
//!
//! `tidepool-runtime/tests/realm_varid_pinning.rs` drives the same property
//! through `ResidentSession::run`, where `free_vars` computes the referenced
//! slice; this test's `referenced` slice is HAND-WIRED, so it proves the
//! table itself enforces the property but not that any production caller
//! computes the right slice.

use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_codegen::old_space::RootSlot;
use tidepool_repr::{BindingName, Generation, SessionModule, SessionVarId, VarId};

/// High-byte tag a real Option-C session binder carries (`stableVarId`,
/// 0xFE-tagged external).
const EXTERNAL_TAG: u64 = 0xFE;

fn external_var_id(key: u64) -> VarId {
    VarId((EXTERNAL_TAG << 56) | (key & ((1u64 << 56) - 1)))
}

/// A fake registered slot. This test never loads through it (no bind/run
/// happens), so a dangling box address is fine.
fn fake_slot(boxed: &mut *mut u8) -> RootSlot {
    // SAFETY: test-only; never dereferenced.
    unsafe { RootSlot::new(boxed as *mut *mut u8) }
}

fn entry(name: &str, var: VarId, gen: u64, slot: RootSlot) -> BindingEntry {
    BindingEntry {
        defining_expr: None,
        name: BindingName(name.to_string()),
        id: SessionVarId::from_var(var),
        module: SessionModule::val(Generation(gen)),
        value: BoundValue::Tier0Forced(slot),
        type_display: Some("Int".to_string()),
        // Flat-session fixture; `bind` rewrites this to ROOT anyway.
        scope: tidepool_codegen::scope::ScopeId::ROOT,
    }
}

#[test]
fn two_scopes_colliding_on_display_name_never_leak_sessionvarids_into_each_others_env() {
    let mut ax: *mut u8 = std::ptr::null_mut();
    let mut atmp: *mut u8 = std::ptr::null_mut();
    let mut bx: *mut u8 = std::ptr::null_mut();
    let mut btmp: *mut u8 = std::ptr::null_mut();
    let (s_ax, s_atmp, s_bx, s_btmp) = (
        fake_slot(&mut ax),
        fake_slot(&mut atmp),
        fake_slot(&mut bx),
        fake_slot(&mut btmp),
    );

    let mut table = BindingTable::new();

    // Scope A binds `x` and `tmp`.
    let a_x = external_var_id(0xA001);
    let a_tmp = external_var_id(0xA002);
    table.bind(entry("x", a_x, 1, s_ax));
    table.bind(entry("tmp", a_tmp, 1, s_atmp));

    // Scope B independently binds `x` and `tmp` — COLLIDES on display name
    // with scope A (both share ONE `BindingTable`, as two realms would), but
    // each gets its own freshly-minted `SessionVarId` (fresh-id minting is
    // the other half of the isolation property, alongside D9's narrowing
    // this test pins).
    let b_x = external_var_id(0xB001);
    let b_tmp = external_var_id(0xB002);
    table.bind(entry("x", b_x, 2, s_bx));
    table.bind(entry("tmp", b_tmp, 2, s_btmp));

    assert_ne!(a_x, b_x, "fresh minting must not collide across scopes");
    assert_ne!(a_tmp, b_tmp, "fresh minting must not collide across scopes");

    // Direction 1: a fragment compiled against scope A's referenced ids must
    // see NONE of scope B's ids, despite both living in the same `live` map
    // under colliding names.
    let env_a = table.seed_external_env(&[a_x, a_tmp]);
    assert!(env_a.get(a_x).is_some(), "A's env must contain A's x");
    assert!(env_a.get(a_tmp).is_some(), "A's env must contain A's tmp");
    assert!(
        env_a.get(b_x).is_none(),
        "A's env must NOT contain B's x — cross-scope isolation"
    );
    assert!(
        env_a.get(b_tmp).is_none(),
        "A's env must NOT contain B's tmp — cross-scope isolation"
    );

    // Direction 2 (the leak is not symmetric under all plausible
    // regressions — e.g. a cache keyed on display name rather than
    // SessionVarId could leak only one direction).
    let env_b = table.seed_external_env(&[b_x, b_tmp]);
    assert!(env_b.get(b_x).is_some(), "B's env must contain B's x");
    assert!(env_b.get(b_tmp).is_some(), "B's env must contain B's tmp");
    assert!(
        env_b.get(a_x).is_none(),
        "B's env must NOT contain A's x — cross-scope isolation"
    );
    assert!(
        env_b.get(a_tmp).is_none(),
        "B's env must NOT contain A's tmp — cross-scope isolation"
    );
}
