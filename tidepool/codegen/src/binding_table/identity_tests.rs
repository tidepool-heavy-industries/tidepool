use super::*;
use crate::prepared_program::PreparedHandle;
use crate::suspension::ValueHandle;
use tidepool_repr::execution_schema::{RuntimeRep, SymbolIdentity};
use tidepool_repr::Generation;

fn entry(name: &str, generation: u64, slot: &mut *mut u8) -> BindingEntry {
    BindingEntry {
        name: BindingName(name.into()),
        id: SessionVarId::from_extract(generation),
        module: SessionModule::val(Generation(generation)),
        value: BoundValue {
            root: unsafe { RootSlot::new(slot) },
            handle: PreparedHandle::new(ValueHandle(generation), RuntimeRep::LiftedRef),
            identity: SymbolIdentity {
                unit: "test".into(),
                module: format!("Val.G{generation}"),
                namespace: "value".into(),
                occurrence: name.into(),
                record_parent: None,
            },
        },
        type_display: Some("Int".into()),
        defining_expr: Some("42".into()),
        scope: ScopeId::ROOT,
    }
}

fn assert_indexes(table: &BindingTable) {
    let mut owned = HashMap::<ScopeId, HashSet<SessionVarId>>::new();
    let mut modules = HashMap::<SessionModule, usize>::new();
    for entry in table.live.values() {
        owned.entry(entry.scope).or_default().insert(entry.id);
        *modules.entry(entry.module).or_default() += 1;
    }
    assert_eq!(table.owned, owned);
    assert_eq!(table.modules, modules);
    let mut source_owned = HashMap::<ScopeId, HashSet<SourceLeaseKey>>::new();
    for (key, lease) in &table.source_instances {
        source_owned
            .entry(lease.owner)
            .or_default()
            .insert(key.clone());
    }
    assert_eq!(table.source_owned, source_owned);
}

#[test]
fn immutable_identity_reinsertion_is_idempotent_and_conflicts_are_atomic() {
    let mut tree = ScopeTree::new();
    let scope = tree.mint_isolated();
    let foreign = tree.mint_isolated();
    let mut table = BindingTable::new();
    let mut slot = std::ptr::null_mut();
    let id = table.bind_in(scope, entry("answer", 1, &mut slot)).unwrap();
    let revision = table.mutation_revision();
    let witness = table.scope_witness(&tree, scope);
    assert_eq!(table.bind_in(scope, entry("answer", 1, &mut slot)), Ok(id));
    assert_eq!(table.mutation_revision(), revision);
    assert_eq!(table.scope_witness(&tree, scope), witness);
    for change in 0..8 {
        let mut changed = entry("answer", 1, &mut slot);
        let changed_scope = if change == 0 { foreign } else { scope };
        match change {
            1 => changed.name = BindingName("other".into()),
            2 => changed.module = SessionModule::val(Generation(2)),
            3 => changed.value.root = unsafe { RootSlot::new(std::ptr::null_mut()) },
            4 => changed.value.handle = PreparedHandle::new(ValueHandle(2), RuntimeRep::LiftedRef),
            5 => changed.value.identity.record_parent = Some("Record".into()),
            6 => changed.type_display = None,
            7 => changed.defining_expr = None,
            _ => {}
        }
        assert_eq!(
            table.bind_in(changed_scope, changed),
            Err(BindingIdentityError { id })
        );
        assert_eq!(table.mutation_revision(), revision);
        assert_eq!(table.scope_witness(&tree, scope), witness);
        assert_eq!(table.resolve_in(&tree, scope, "answer").unwrap().id, id);
        assert!(table.resolve_in(&tree, foreign, "answer").is_none());
        assert_indexes(&table);
    }
    table.remove_current_in(scope, "answer");
    let hidden_revision = table.mutation_revision();
    table.bind_in(scope, entry("answer", 1, &mut slot)).unwrap();
    assert!(table.resolve_in(&tree, scope, "answer").is_none());
    assert_eq!(table.mutation_revision(), hidden_revision);
}

#[test]
fn witnesses_follow_owner_mutations_without_invalidating_frozen_siblings() {
    let mut tree = ScopeTree::new();
    let parent = tree.mint_isolated();
    let child = tree.mint_isolated();
    let sibling = tree.mint_isolated();
    let mut table = BindingTable::new();
    let mut first_slot = std::ptr::null_mut();
    let first = table
        .bind_in(parent, entry("answer", 1, &mut first_slot))
        .unwrap();
    table.seed_detached_scope(&tree, parent, child);
    let frozen = table.scope_witness(&tree, child);
    let parent_before = table.scope_witness(&tree, parent);
    let mut later_slot = std::ptr::null_mut();
    table
        .bind_in(parent, entry("answer", 2, &mut later_slot))
        .unwrap();
    assert_ne!(table.scope_witness(&tree, parent), parent_before);
    assert_eq!(table.scope_witness(&tree, child), frozen);
    let mut sibling_slot = std::ptr::null_mut();
    table
        .bind_in(sibling, entry("answer", 3, &mut sibling_slot))
        .unwrap();
    assert_eq!(table.scope_witness(&tree, child), frozen);
    let leased_revision = table.mutation_revision();
    let shares = table.acquire_leases([first]);
    table.release_leases(shares);
    assert_eq!(table.mutation_revision(), leased_revision);
    let child_before = table.scope_witness(&tree, child);
    table.remove_current_in(child, "answer");
    assert_ne!(table.scope_witness(&tree, child), child_before);
    let hidden = table.scope_witness(&tree, child);
    table.remove_current_in(child, "answer");
    assert_eq!(table.scope_witness(&tree, child), hidden);
    let mut alias_slot = std::ptr::null_mut();
    let alias = entry("page", 4, &mut alias_slot);
    table.bind_alias_in(parent, alias, first).unwrap();
    let dependency_before = table.scope_witness(&tree, child);
    assert!(table.retain_scope_dependencies(&tree, parent, child));
    assert_ne!(table.scope_witness(&tree, child), dependency_before);
    let before_retirement = table.scope_witness(&tree, child);
    table.drain_scope(parent);
    assert_eq!(table.scope_witness(&tree, child), before_retirement);
    assert!(table.get(first).is_some());
    assert_indexes(&table);
    table.drain_scope(child);
    assert!(table.get(first).is_none());
    assert_indexes(&table);
}

#[test]
fn observation_promotion_and_final_release_keep_witnesses_and_indexes_complete() {
    let mut tree = ScopeTree::new();
    let scope = tree.mint_isolated();
    let target = tree.mint_isolated();
    let mut table = BindingTable::new();
    let mut slot = std::ptr::null_mut();
    let id = table
        .bind_in(scope, entry("observation", 1, &mut slot))
        .unwrap();
    let before = table.scope_witness(&tree, scope);
    table.save_observation(id, &[], 1);
    assert_ne!(table.scope_witness(&tree, scope), before);
    let before = table.scope_witness(&tree, scope);
    table.preserve_observations(&[id.var()]);
    assert_ne!(table.scope_witness(&tree, scope), before);
    let before = table.scope_witness(&tree, target);
    let revision = table.mutation_revision();
    assert!(table
        .promote_exact_bindings_in(scope, target, &[SessionVarId::from_extract(99)])
        .is_err());
    assert_eq!(table.mutation_revision(), revision);
    table
        .promote_exact_bindings_in(scope, target, &[id])
        .unwrap();
    assert_ne!(table.scope_witness(&tree, target), before);
    table.drain_scope(scope);
    assert!(table.get(id).is_some());
    assert_indexes(&table);
    table.drain_scope(target);
    assert!(table.get(id).is_none());
    assert_indexes(&table);
}

#[test]
fn exhausted_revisions_disable_witnesses_without_reusing_an_identity() {
    let mut tree = ScopeTree::new();
    let scope = tree.mint_isolated();
    let mut table = BindingTable::new();
    table.revision = u64::MAX;
    let mut slot = std::ptr::null_mut();
    let id = table.bind_in(scope, entry("answer", 1, &mut slot)).unwrap();
    assert_eq!(table.mutation_revision(), None);
    assert_eq!(table.scope_witness(&tree, scope), None);
    assert_eq!(table.bind_in(scope, entry("answer", 1, &mut slot)), Ok(id));
    assert_eq!(table.mutation_revision(), None);
    assert_indexes(&table);
}

#[test]
fn frozen_exact_ids_cannot_gain_later_ancestor_dependencies_or_sibling_aliases() {
    let mut tree = ScopeTree::new();
    let parent = tree.mint_isolated();
    let child = tree.mint_isolated();
    let sibling = tree.mint_isolated();
    let mut table = BindingTable::new();
    let mut parent_slot = std::ptr::null_mut();
    let parent_id = table
        .bind_in(parent, entry("parent", 1, &mut parent_slot))
        .unwrap();
    let mut sibling_slot = std::ptr::null_mut();
    let sibling_entry = entry("sibling", 2, &mut sibling_slot);
    let sibling_id = sibling_entry.id;
    let sibling_identity = sibling_entry.value.identity.clone();
    table.bind_in(sibling, sibling_entry).unwrap();
    table.save_observation(sibling_id, &[], 1);
    table.seed_detached_scope(&tree, parent, child);
    let witness = table.scope_witness(&tree, child);
    table.save_observation(parent_id, &[sibling_id.var()], 1);
    assert_eq!(table.scope_witness(&tree, child), witness);
    assert_eq!(
        table.scope_reachable_binding_ids(&tree, child),
        vec![parent_id]
    );
    assert!(table
        .scope_reachable_binding_ids(&tree, parent)
        .contains(&sibling_id));
    let mut alias_slot = std::ptr::null_mut();
    let mut alias = entry("foreign_alias", 3, &mut alias_slot);
    alias.module = SessionModule::val(Generation(1));
    alias.value.identity = sibling_identity.clone();
    table.bind_in(sibling, alias).unwrap();
    assert!(table
        .resolve_exact_prepared_in(&tree, child, &sibling_identity, 1)
        .is_none());
    assert_eq!(table.scope_witness(&tree, child), witness);
    let grandchild = tree.mint_isolated();
    table.seed_detached_scope(&tree, child, grandchild);
    assert_eq!(
        table.scope_reachable_binding_ids(&tree, grandchild),
        vec![parent_id]
    );
    let target = tree.mint_isolated();
    let empty_parent = tree.mint_isolated();
    table.seed_detached_scope(&tree, empty_parent, target);
    assert!(table.retain_scope_dependencies(&tree, child, target));
    assert_eq!(
        table.scope_reachable_binding_ids(&tree, target),
        vec![parent_id]
    );
}

#[test]
fn unseeded_witness_tracks_ancestor_frames_and_last_removal() {
    let mut tree = ScopeTree::new();
    let parent = tree.mint_isolated();
    let child = tree.mint_child(parent).unwrap();
    let mut table = BindingTable::new();
    let before = table.scope_witness(&tree, child);
    let mut slot = std::ptr::null_mut();
    let id = table
        .bind_in(parent, entry("parent", 1, &mut slot))
        .unwrap();
    let named = table.scope_witness(&tree, child);
    assert_ne!(named, before);
    table.remove_live(id).unwrap();
    assert_ne!(table.scope_witness(&tree, child), named);
    assert_indexes(&table);
}

#[test]
fn observation_witness_covers_mutable_cross_scope_dependency_metadata() {
    let mut tree = ScopeTree::new();
    let a = tree.mint_isolated();
    let b = tree.mint_isolated();
    let c = tree.mint_isolated();
    let mut table = BindingTable::new();
    let mut a_slot = std::ptr::null_mut();
    let mut b_slot = std::ptr::null_mut();
    let mut c_slot = std::ptr::null_mut();
    let a_id = table.bind_in(a, entry("a", 1, &mut a_slot)).unwrap();
    let b_id = table.bind_in(b, entry("b", 2, &mut b_slot)).unwrap();
    let c_id = table.bind_in(c, entry("c", 3, &mut c_slot)).unwrap();
    table.save_observation(c_id, &[], 1);
    table.save_observation(b_id, &[], 1);
    table.save_observation(a_id, &[b_id.var()], 1);
    let before = table.scope_witness(&tree, a);
    assert!(!table.scope_reachable_binding_ids(&tree, a).contains(&c_id));
    table.save_observation(b_id, &[c_id.var()], 1);
    assert_ne!(table.scope_witness(&tree, a), before);
    assert!(table.scope_reachable_binding_ids(&tree, a).contains(&c_id));
    let before = table.scope_witness(&tree, a);
    table.preserve_observations(&[b_id.var()]);
    assert_ne!(table.scope_witness(&tree, a), before);
    assert!(!table.scope_reachable_binding_ids(&tree, a).contains(&c_id));
    assert_indexes(&table);
}
