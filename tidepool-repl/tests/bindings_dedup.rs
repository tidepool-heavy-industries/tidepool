//! Pure unit test for the `:bindings` dedup property, demoted out of
//! `shadow_rebind.rs`'s `bindings_after_rebind_lists_once` (session-test-review
//! quick win #3): the JSON view's "list a rebound name once" guarantee comes
//! straight from `BindingTable::iter_current()` deduping by name over its
//! in-memory `current` map — no session, no compile, needed to prove it.

use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_codegen::old_space::RootSlot;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{BindingName, Generation, SessionModule, SessionVarId};

/// A `RootSlot` wrapping a stable, valid, non-null address that this test
/// never dereferences — `iter_current`'s dedup-by-name never touches the
/// value half of a binding, only its name/module/id.
fn fake_slot() -> RootSlot {
    let ptr = Box::into_raw(Box::new(std::ptr::null_mut::<u8>()));
    unsafe { RootSlot::new(ptr) }
}

fn entry(name: &str, gen: u64, raw_id: u64) -> BindingEntry {
    BindingEntry {
        name: BindingName(name.to_string()),
        id: SessionVarId::from_extract(raw_id),
        module: SessionModule::val(Generation(gen)),
        value: BoundValue::Tier0Forced(fake_slot()),
        type_display: Some("Int".to_string()),
        defining_expr: None,
        scope: ScopeId::ROOT,
    }
}

/// After a rebind (same name, newer gen, fresh id), `iter_current()` lists the
/// name exactly ONCE — the newest gen, not both. Mirrors the property
/// `shadow_rebind.rs`'s `bindings_after_rebind_lists_once` used to prove via a
/// live 3-turn session; this is the pure-Rust layer `:bindings` actually reads.
#[test]
fn iter_current_lists_a_rebound_name_exactly_once() {
    let mut table = BindingTable::new();
    table.bind(entry("x", 1, 101));
    table.bind(entry("x", 2, 102));

    let occurrences = table.iter_current().filter(|(n, _)| n.0 == "x").count();
    assert_eq!(
        occurrences, 1,
        "iter_current should list a rebound name exactly once (newest gen), got {occurrences}"
    );
    let (_, e) = table
        .iter_current()
        .find(|(n, _)| n.0 == "x")
        .expect("x should be current");
    assert_eq!(e.module.gen(), Generation(2), "newest gen must win");
}
