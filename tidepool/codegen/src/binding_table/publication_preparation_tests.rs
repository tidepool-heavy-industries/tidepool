use super::*;
use proptest::prelude::*;
use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion};

fn without_allocations(install: impl FnOnce()) {
    super::cost_tests::measure(install).1.assert_nonallocating();
}

fn entry(name: &str, generation: u64) -> BindingEntry {
    super::promotion_tests::entry(name, generation)
}

fn visible(table: &BindingTable, tree: &ScopeTree, scope: ScopeId) -> Vec<(String, SessionVarId)> {
    table
        .iter_current_in(tree, scope)
        .into_iter()
        .map(|(name, entry)| (name.0.clone(), entry.id))
        .collect()
}

#[test]
fn declaration_prepare_cancel_and_commit_preserve_exact_observation_history() {
    let mut tree = ScopeTree::new();
    let root = ScopeId::ROOT;
    let child = tree.mint_child(root).unwrap();
    let mut table = BindingTable::new();
    let old = table.bind_in(root, entry("answer", 1)).unwrap();
    table.seed_scope(&tree, root, child);
    let first = table.bind_in(child, entry("first", 2)).unwrap();
    let second = table.bind_in(child, entry("answer", 3)).unwrap();
    table.save_observation(first, &[], 10);
    table.save_observation(second, &[first.var()], 10);
    table.save_observation(first, &[second.var()], 10);
    let owner = CachedHomeOwner {
        unit: "unit".into(),
        module: "Home".into(),
        module_version: ModuleVersion([1; 32]),
        skinny_iface_sha256: [2; 32],
        product_sha256: [3; 32],
    };
    let before = table.scope_snapshot(&tree, child).unwrap();
    let before_visible = visible(&table, &tree, child);
    let before_revision = table.mutation_revision();
    let origin = table
        .prepare_source_owner_origin_in(&tree, child, owner.clone())
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_in(
            child,
            &[first.var()],
            &["answer".into(), "answer".into()],
        )
        .unwrap();
    assert_eq!(before, table.scope_snapshot(&tree, child).unwrap());
    assert_eq!(before_visible, visible(&table, &tree, child));
    assert_eq!(before_revision, table.mutation_revision());
    drop((origin, changes));
    assert_eq!(before, table.scope_snapshot(&tree, child).unwrap());
    let origin = table
        .prepare_source_owner_origin_in(&tree, child, owner)
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_in(child, &[first.var()], &["answer".into()])
        .unwrap();
    let prospective = table
        .prepared_scope_snapshot_in(&tree, child, Some(&origin), Some(&changes))
        .unwrap();
    let prospective_visible: Vec<_> = table
        .prepared_iter_current_in(&tree, child, &changes)
        .unwrap()
        .into_iter()
        .map(|(name, entry)| (name.0, entry.id))
        .collect();
    without_allocations(|| {
        table.commit_source_owner_origin(origin);
        table.commit_declaration_binding_changes(changes);
    });
    assert_eq!(prospective, table.scope_snapshot(&tree, child).unwrap());
    assert_eq!(prospective_visible, visible(&table, &tree, child));
    assert!(table.resolve_in(&tree, child, "answer").is_none());
    assert_eq!(table.resolve_in(&tree, root, "answer").unwrap().id, old);
    assert!(!table.observations.contains_key(&first));
    assert!(!table.observations.contains_key(&second));
    assert!(table.live.contains_key(&first) && table.live.contains_key(&second));
}

#[test]
fn promotion_shadowing_keeps_hidden_dependency_custody_without_install_allocations() {
    let mut tree = ScopeTree::new();
    let source = tree.mint_isolated();
    let target = tree.mint_isolated();
    let mut table = BindingTable::new();
    let root = table.bind_in(source, entry("root", 1)).unwrap();
    let alias = table
        .bind_alias_in(source, entry("answer", 2), root)
        .unwrap()
        .0;
    let before = table.scope_snapshot(&tree, target).unwrap();
    let promotion = table
        .prepare_exact_binding_promotion_in(source, target, &[alias])
        .unwrap();
    let promotion = table
        .prepare_binding_publication_removals(promotion, &["answer".into()])
        .unwrap();
    assert_eq!(before, table.scope_snapshot(&tree, target).unwrap());
    assert_eq!(table.lease_count(root), 0);
    drop(promotion);
    assert_eq!(table.lease_count(alias), 0);
    let promotion = table
        .prepare_exact_binding_promotion_in(source, target, &[alias])
        .unwrap();
    let promotion = table
        .prepare_binding_publication_removals(promotion, &["answer".into()])
        .unwrap();
    without_allocations(|| table.commit_exact_binding_promotion(promotion));
    assert!(table.resolve_in(&tree, target, "answer").is_none());
    assert_eq!(table.lease_count(root), 1);
    assert_eq!(table.lease_count(alias), 1);
    assert!(table.drain_scope(source).is_empty());
    assert_eq!(
        table.scope_reachable_binding_ids(&tree, target),
        vec![root, alias]
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn prepared_promotion_histories_match_visible_model(
        history in prop::collection::vec((0u8..4, any::<bool>(), any::<bool>()), 1..48)
    ) {
        let mut tree = ScopeTree::new();
        let target = tree.mint_isolated();
        let mut table = BindingTable::new();
        let mut model = HashMap::new();
        for (step, (slot, commit, remove)) in history.into_iter().enumerate() {
            let source = tree.mint_isolated();
            let name = format!("name{slot}");
            let id = table.bind_in(source, entry(&name, step as u64 + 1)).unwrap();
            let before = visible(&table, &tree, target);
            let snapshot = table.scope_snapshot(&tree, target).unwrap();
            let revision = table.mutation_revision();
            let token = table.prepare_exact_binding_promotion_in(source, target, &[id]).unwrap();
            let removals = if remove { vec![name.clone()] } else { Vec::new() };
            let token = table.prepare_binding_publication_removals(token, &removals).unwrap();
            prop_assert_eq!(visible(&table, &tree, target), before);
            prop_assert_eq!(table.scope_snapshot(&tree, target).unwrap(), snapshot);
            prop_assert_eq!(table.mutation_revision(), revision);
            if commit {
                without_allocations(|| table.commit_exact_binding_promotion(token));
                if remove { model.remove(&name); } else { model.insert(name.clone(), id); }
                prop_assert_eq!(table.lease_count(id), 1);
            } else {
                drop(token);
                prop_assert_eq!(table.lease_count(id), 0);
            }
            let mut expected: Vec<_> = model.iter().map(|(name, id)| (name.clone(), *id)).collect();
            expected.sort_by(|a, b| a.0.cmp(&b.0));
            prop_assert_eq!(visible(&table, &tree, target), expected);
        }
    }
}

#[test]
fn cancelled_promotion_then_name_removal_does_not_grow_revision_index() {
    let mut tree = ScopeTree::new();
    let target = tree.mint_isolated();
    let mut table = BindingTable::new();
    for generation in 1..=2 {
        let source = tree.mint_isolated();
        let id = table.bind_in(source, entry("answer", generation)).unwrap();
        let prepared = table
            .prepare_exact_binding_promotion_in(source, target, &[id])
            .unwrap();
        let prepared = table
            .prepare_binding_publication_removals(prepared, &["answer".into()])
            .unwrap();
        if generation == 1 {
            drop(prepared);
        } else {
            without_allocations(|| table.commit_exact_binding_promotion(prepared));
        }
    }
    assert!(visible(&table, &tree, target).is_empty());
}

#[test]
fn prepared_detached_capture_cancellation_and_install_preserve_exact_future_tip() {
    let mut tree = ScopeTree::new();
    let parent = tree.mint_isolated();
    let foreign = tree.mint_isolated();
    let older = tree.mint_isolated();
    let child = tree.mint_isolated();
    let mut table = BindingTable::new();
    let hidden = table.bind_in(parent, entry("answer", 1)).unwrap();
    let dependency = table.bind_in(foreign, entry("foreign", 2)).unwrap();
    let observation = table.bind_in(parent, entry("observation", 3)).unwrap();
    table.save_observation(dependency, &[], 10);
    table.save_observation(observation, &[dependency.var()], 10);
    table.seed_detached_scope(&tree, parent, older);
    assert!(table.tips[&older].retained.contains(&dependency));
    let owner = CachedHomeOwner {
        unit: "unit".into(),
        module: "Future".into(),
        module_version: ModuleVersion([4; 32]),
        skinny_iface_sha256: [5; 32],
        product_sha256: [6; 32],
    };
    let before = table.scope_snapshot(&tree, parent).unwrap();
    let counts = [hidden, dependency, observation].map(|id| table.lease_count(id));
    let next_tip = table.next_tip;
    let origin = table
        .prepare_source_owner_origin_in(&tree, parent, owner.clone())
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_in(parent, &[observation.var()], &["answer".into()])
        .unwrap();
    let capture = table
        .prepare_seed_detached_scope_for_declaration(&tree, parent, child, &origin, &changes)
        .unwrap();
    assert_eq!(before, table.scope_snapshot(&tree, parent).unwrap());
    assert_eq!(
        counts,
        [hidden, dependency, observation].map(|id| table.lease_count(id))
    );
    assert_eq!(next_tip, table.next_tip);
    assert!(table.tip_id(child).is_none());
    drop(capture);
    assert_eq!(
        counts,
        [hidden, dependency, observation].map(|id| table.lease_count(id))
    );
    let capture = table
        .prepare_seed_detached_scope_for_declaration(&tree, parent, child, &origin, &changes)
        .unwrap();
    without_allocations(|| {
        table.commit_source_owner_origin(origin);
        table.commit_declaration_binding_changes(changes);
        table.commit_prepared_detached_scope(capture);
    });
    assert!(table.resolve_in(&tree, child, "answer").is_none());
    assert_eq!(
        table.resolve_in(&tree, child, "observation").unwrap().id,
        observation
    );
    assert!(table.tips[&child].retained.contains(&hidden));
    assert!(!table.tips[&child].retained.contains(&dependency));
    let selection = table.source_domain_selection_in(&tree, child).unwrap();
    assert_eq!(
        selection
            .domain_for_owner(table.source_selection[&child].current, &owner)
            .unwrap(),
        table.source_selection[&child].current
    );
    assert_ne!(
        table.source_selection[&child].current,
        table.source_selection[&parent].current
    );
    let origin = table
        .prepare_source_owner_origin_in(&tree, parent, owner)
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_in(parent, &[], &[])
        .unwrap();
    assert!(matches!(
        table.prepare_seed_detached_scope_for_declaration(&tree, parent, child, &origin, &changes),
        Err(BindingPromotionError::UnverifiableSourceSelection)
    ));
    table.drain_scope(older);
    assert_eq!(table.lease_count(dependency), 0);
    assert!(table.drain_scope(parent).is_empty());
    assert_eq!(table.drain_scope(child).len(), 2);
    assert!(table.get(dependency).is_some());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn prospective_detached_histories_keep_cancellation_and_repeated_custody_exact(
        history in prop::collection::vec((0u8..4, any::<bool>(), any::<bool>()), 1..32)
    ) {
        let mut tree = ScopeTree::new();
        let parent = tree.mint_isolated();
        let mut table = BindingTable::new();
        let mut captured = Vec::new();
        let mut ids = Vec::new();
        for (step, (slot, commit, remove)) in history.into_iter().enumerate() {
            let name = format!("name{slot}");
            let id = table.bind_in(parent, entry(&name, step as u64 + 1)).unwrap();
            ids.push(id);
            let owner = CachedHomeOwner {
                unit: "unit".into(), module: format!("Home{step}"), module_version: ModuleVersion([step as u8; 32]),
                skinny_iface_sha256: [1; 32], product_sha256: [2; 32],
            };
            let child = tree.mint_isolated();
            let origin = table.prepare_source_owner_origin_in(&tree, parent, owner.clone()).unwrap();
            let removals = if remove { vec![name] } else { Vec::new() };
            let changes = table.prepare_declaration_binding_changes_in(parent, &[], &removals).unwrap();
            let before = table.scope_snapshot(&tree, parent).unwrap();
            let counts: Vec<_> = ids.iter().map(|id| table.lease_count(*id)).collect();
            let next_tip = table.next_tip;
            let token = table.prepare_seed_detached_scope_for_declaration(&tree, parent, child, &origin, &changes).unwrap();
            prop_assert_eq!(table.scope_snapshot(&tree, parent).unwrap(), before);
            prop_assert_eq!(ids.iter().map(|id| table.lease_count(*id)).collect::<Vec<_>>(), counts);
            prop_assert_eq!(table.next_tip, next_tip);
            if commit {
                let prospective: Vec<_> = table.prepared_iter_current_in(&tree, parent, &changes).unwrap().into_iter()
                    .map(|(name, entry)| (name.0, entry.id)).collect();
                without_allocations(|| {
                    table.commit_source_owner_origin(origin);
                    table.commit_declaration_binding_changes(changes);
                    table.commit_prepared_detached_scope(token);
                });
                prop_assert_eq!(visible(&table, &tree, child), prospective);
                captured.push(child);
                for id in &ids { prop_assert_eq!(table.lease_count(*id), 1); }
                let duplicate = tree.mint_isolated();
                let origin = table.prepare_source_owner_origin_in(&tree, parent, owner).unwrap();
                let changes = table.prepare_declaration_binding_changes_in(parent, &[], &[]).unwrap();
                let token = table.prepare_seed_detached_scope_for_declaration(&tree, parent, duplicate, &origin, &changes).unwrap();
                without_allocations(|| table.commit_prepared_detached_scope(token));
                prop_assert!(Arc::ptr_eq(&table.tips[&child].leases, &table.tips[&duplicate].leases));
                for id in &ids { prop_assert_eq!(table.lease_count(*id), 1); }
                captured.push(duplicate);
            } else {
                drop((origin, changes, token));
                prop_assert_eq!(table.lease_count(id), 0);
            }
        }
        table.drain_scope(parent);
        for child in captured { table.drain_scope(child); }
        prop_assert!(table.is_empty());
    }
}
