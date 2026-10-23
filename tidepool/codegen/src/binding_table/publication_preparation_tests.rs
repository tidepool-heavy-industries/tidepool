use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
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

fn declaration_owner(version: u8) -> CachedHomeOwner {
    CachedHomeOwner {
        unit: "unit".into(),
        module: "Declaration".into(),
        module_version: ModuleVersion([version; 32]),
        skinny_iface_sha256: [version; 32],
        product_sha256: [version; 32],
    }
}

#[test]
fn declaration_hidden_custody_survives_guard_retirement_and_publication_without_names() {
    let mut tree = ScopeTree::new();
    let private = tree.mint_isolated();
    let public = tree.mint_isolated();
    let mut table = BindingTable::new();
    let dependency = table.bind_in(private, entry("dependency", 200)).unwrap();
    let input = table.bind_in(private, entry("hidden_input", 201)).unwrap();
    table.save_observation(dependency, &[], 10);
    table.save_observation(input, &[dependency.var()], 10);
    let guard = table.acquire_leases([input]);
    let before = table.scope_witness(&tree, private).unwrap();
    let owner = declaration_owner(1);
    let mut origin = table
        .prepare_source_owner_origin_in(&tree, private, owner.clone())
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            private,
            &mut origin,
            &[input.var()],
            &["hidden_input".into()],
            &[input],
        )
        .unwrap();
    let projected = table
        .prepared_declaration_projection(&tree, Some(&origin), &changes)
        .unwrap();
    let witness = projected.scope_witness().clone();
    assert!(projected
        .visible_changes()
        .all(|(_, entry)| entry.is_none()));
    assert_eq!(table.scope_witness(&tree, private), Some(before));
    assert_eq!(table.lease_count(input), 1);
    let snapshot = table
        .prepared_scope_snapshot_in(&tree, private, Some(&origin), Some(&changes))
        .unwrap();
    without_allocations(|| {
        table.commit_source_owner_origin(origin);
        table.commit_declaration_binding_changes(changes);
    });
    assert_eq!(table.scope_witness(&tree, private), Some(witness));
    assert_eq!(table.scope_snapshot(&tree, private), Ok(snapshot));
    assert_eq!(table.lease_count(input), 2);
    assert_eq!(table.lease_count(dependency), 2);
    assert!(table.resolve_in(&tree, private, "hidden_input").is_none());
    assert!(table.retire_owner(input).is_none());
    assert!(table.release_leases(guard).is_empty());
    assert_eq!(table.lease_count(input), 1);
    assert_eq!(table.lease_count(dependency), 1);
    // Repeating an authored origin keeps the same frozen support and scoped
    // lease. Recommitting the origin still has its ordinary revision advance.
    let mut origin = table
        .prepare_source_owner_origin_in(&tree, private, owner.clone())
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            private,
            &mut origin,
            &[],
            &["missing_declaration".into()],
            &[input],
        )
        .unwrap();
    let before = table.scope_witness(&tree, private).unwrap();
    let projected = table
        .prepared_declaration_projection(&tree, Some(&origin), &changes)
        .unwrap()
        .scope_witness()
        .clone();
    assert_ne!(projected, before);
    without_allocations(|| {
        table.commit_source_owner_origin(origin);
        table.commit_declaration_binding_changes(changes);
    });
    assert_eq!(table.scope_witness(&tree, private), Some(projected));
    assert_eq!(table.lease_count(input), 1);
    let publication = table
        .prepare_authored_source_publication_in(&tree, private, public, &[], &[], &[owner])
        .unwrap();
    without_allocations(|| table.commit_exact_binding_promotion(publication));
    assert!(table.iter_current_in(&tree, public).is_empty());
    assert_eq!(table.lease_count(input), 2);
    table.drain_scope(private);
    assert!(table.get(input).is_some());
    assert!(table.get(dependency).is_some());
    assert_eq!(table.lease_count(input), 1);
    let released = table.drain_scope(public);
    assert_eq!(released.len(), 2);
    assert!(table.get(input).is_none());
    assert!(table.get(dependency).is_none());
}

#[test]
fn declaration_custody_preparation_cancel_foreign_and_overflow_leave_roots_unchanged() {
    let mut tree = ScopeTree::new();
    let scope = tree.mint_isolated();
    let foreign = tree.mint_isolated();
    let mut table = BindingTable::new();
    let id = table.bind_in(scope, entry("input", 210)).unwrap();
    let foreign_id = table.bind_in(foreign, entry("input", 211)).unwrap();
    let before = table.scope_witness(&tree, scope).unwrap();
    let mut origin = table
        .prepare_source_owner_origin_in(&tree, scope, declaration_owner(3))
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            scope,
            &mut origin,
            &[],
            &[],
            &[id],
        )
        .unwrap();
    assert_eq!(table.lease_count(id), 0);
    let child = tree.mint_isolated();
    assert!(matches!(
        table.prepare_seed_detached_scope_for_declaration(&tree, scope, child, &origin, &changes),
        Err(BindingPromotionError::UnverifiableSourceSelection)
    ));
    drop(changes);
    assert_eq!(table.scope_witness(&tree, scope), Some(before.clone()));
    for ids in [vec![foreign_id], vec![SessionVarId::from_extract(999)]] {
        assert!(matches!(
            table.prepare_declaration_binding_changes_with_custody_in(
                &tree,
                scope,
                &mut origin,
                &[],
                &[],
                &ids,
            ),
            Err(BindingPromotionError::MissingOrForeignBinding)
        ));
    }
    assert_eq!(table.scope_witness(&tree, scope), Some(before.clone()));
    table.leases.insert(id, usize::MAX);
    assert!(matches!(
        table.prepare_declaration_binding_changes_with_custody_in(
            &tree,
            scope,
            &mut origin,
            &[],
            &[],
            &[id],
        ),
        Err(BindingPromotionError::LeaseCountExhausted)
    ));
    assert_eq!(table.lease_count(id), usize::MAX);
    assert_eq!(table.scope_witness(&tree, scope), Some(before));
    table.leases.remove(&id);
    table.revision = u64::MAX;
    let mut origin = table
        .prepare_source_owner_origin_in(&tree, scope, declaration_owner(3))
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            scope,
            &mut origin,
            &[],
            &[],
            &[id],
        )
        .unwrap();
    assert!(matches!(
        table.prepared_declaration_projection(&tree, Some(&origin), &changes),
        Err(BindingPromotionError::MutationWitnessExhausted { advances: 2, .. })
    ));
    assert_eq!(table.lease_count(id), 0);
    drop(changes);
    assert!(table.get(id).is_some());
}

#[test]
fn declaration_custody_accepts_exact_ancestor_frozen_and_promoted_support() {
    let mut tree = ScopeTree::new();
    let ancestor = tree.mint_isolated();
    let child = tree.mint_child(ancestor).unwrap();
    let frozen = tree.mint_isolated();
    let promoted = tree.mint_isolated();
    let mut table = BindingTable::new();
    let id = table.bind_in(ancestor, entry("input", 220)).unwrap();
    table.seed_scope(&tree, ancestor, frozen);
    table
        .promote_exact_bindings_in(ancestor, promoted, &[id])
        .unwrap();
    for scope in [child, frozen, promoted] {
        let mut origin = table
            .prepare_source_owner_origin_in(&tree, scope, declaration_owner(scope.0 as u8))
            .unwrap();
        let changes = table
            .prepare_declaration_binding_changes_with_custody_in(
                &tree,
                scope,
                &mut origin,
                &[],
                &[],
                &[id],
            )
            .unwrap();
        let predicted = table
            .prepared_declaration_projection(&tree, Some(&origin), &changes)
            .unwrap()
            .scope_witness()
            .clone();
        let snapshot = table
            .prepared_scope_snapshot_in(&tree, scope, Some(&origin), Some(&changes))
            .unwrap();
        without_allocations(|| {
            table.commit_source_owner_origin(origin);
            table.commit_declaration_binding_changes(changes);
        });
        assert_eq!(table.scope_witness(&tree, scope), Some(predicted));
        assert_eq!(table.scope_snapshot(&tree, scope), Ok(snapshot));
    }
    assert!(table.drain_scope(ancestor).is_empty());
    assert!(table.get(id).is_some());
    table.drain_scope(child);
    table.drain_scope(frozen);
    assert_eq!(table.drain_scope(promoted).len(), 1);
    assert!(table.get(id).is_none());
}

#[test]
fn declaration_custody_transfer_keeps_captured_edges_after_observation_update() {
    let mut tree = ScopeTree::new();
    let private = tree.mint_isolated();
    let public = tree.mint_isolated();
    let mut table = BindingTable::new();
    let captured = table.bind_in(private, entry("captured", 230)).unwrap();
    let later = table.bind_in(private, entry("later", 231)).unwrap();
    let input = table.bind_in(private, entry("input", 232)).unwrap();
    table.save_observation(captured, &[], 10);
    table.save_observation(later, &[], 10);
    table.save_observation(input, &[captured.var()], 10);
    let owner = declaration_owner(2);
    let mut origin = table
        .prepare_source_owner_origin_in(&tree, private, owner.clone())
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            private,
            &mut origin,
            &[input.var()],
            &[],
            &[input],
        )
        .unwrap();
    table.commit_source_owner_origin(origin);
    table.commit_declaration_binding_changes(changes);
    // Updating mutable observation bookkeeping must not rewrite the already
    // captured declaration's immutable native dependency meaning.
    table.save_observation(input, &[later.var()], 10);
    let publication = table
        .prepare_authored_source_publication_in(&tree, private, public, &[], &[], &[owner])
        .unwrap();
    table.commit_exact_binding_promotion(publication);
    table.retire_owner(input);
    table.drain_scope(private);
    assert!(table.get(input).is_some());
    assert!(table.get(captured).is_some());
    assert!(table.get(later).is_none());
    assert_eq!(table.drain_scope(public).len(), 2);
}

#[test]
fn declaration_frozen_support_pairs_and_cannot_be_overwritten() {
    let mut tree = ScopeTree::new();
    let scope = tree.mint_isolated();
    let target = tree.mint_isolated();
    let mut table = BindingTable::new();
    let first = table.bind_in(scope, entry("first", 240)).unwrap();
    let second = table.bind_in(scope, entry("second", 241)).unwrap();
    let owner = declaration_owner(10);
    let mut x = table
        .prepare_source_owner_origin_in(&tree, scope, owner.clone())
        .unwrap();
    let mut y = table
        .prepare_source_owner_origin_in(&tree, scope, owner.clone())
        .unwrap();
    let dx = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            scope,
            &mut x,
            &[],
            &[],
            &[first],
        )
        .unwrap();
    let dy = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            scope,
            &mut y,
            &[],
            &[],
            &[second],
        )
        .unwrap();
    let original = x.binding_support().unwrap().clone();
    for (origin, delta) in [(&x, &dy), (&y, &dx)] {
        without_allocations(|| {
            assert!(matches!(
                table.prepared_declaration_projection(&tree, Some(origin), delta),
                Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
            ))
        });
    }
    let repeated = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            scope,
            &mut x,
            &[],
            &[],
            &[first],
        )
        .unwrap();
    assert!(Arc::ptr_eq(&original, repeated.support.as_ref().unwrap()));
    assert!(matches!(
        table.prepare_declaration_binding_changes_with_custody_in(
            &tree,
            scope,
            &mut x,
            &[],
            &[],
            &[second]
        ),
        Err(BindingPromotionError::ConflictingSourceOrigin)
    ));
    assert!(Arc::ptr_eq(&original, x.binding_support().unwrap()));
    assert_eq!(table.lease_count(first), 0);
    assert_eq!(table.lease_count(second), 0);
    without_allocations(|| {
        table.commit_source_owner_origin(x);
        table.commit_declaration_binding_changes(dx);
    });
    assert!(matches!(
        table.prepare_authored_source_publication_in(
            &tree,
            scope,
            target,
            &[],
            &[],
            &[declaration_owner(255)]
        ),
        Err(BindingPromotionError::MissingSourceDomain)
    ));
    let mut same = table
        .prepare_source_owner_origin_in(&tree, scope, owner)
        .unwrap();
    let delta = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            scope,
            &mut same,
            &[],
            &[],
            &[first],
        )
        .unwrap();
    assert!(Arc::ptr_eq(&original, delta.support.as_ref().unwrap()));
    assert!(Arc::ptr_eq(&original, same.binding_support().unwrap()));
    drop((same, delta, repeated, y, dy));
    assert_eq!(table.lease_count(first), 1);
    assert_eq!(table.lease_count(second), 0);
}

#[test]
fn authored_promotion_exposes_only_issued_support_and_separate_writes_for_native_preflight() {
    let mut tree = ScopeTree::new();
    let source = tree.mint_isolated();
    let target = tree.mint_isolated();
    let mut table = BindingTable::new();
    let selected = table
        .bind_in(source, entry("hidden_selected", 270))
        .unwrap();
    let compiler_only = table.bind_in(source, entry("compiler_only", 271)).unwrap();
    let ordinary = table.bind_in(source, entry("ordinary", 272)).unwrap();
    let owner = declaration_owner(17);
    let mut origin = table
        .prepare_source_owner_origin_in(&tree, source, owner.clone())
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            source,
            &mut origin,
            &[],
            &["hidden_selected".into()],
            &[selected],
        )
        .unwrap();
    table.commit_source_owner_origin(origin);
    table.commit_declaration_binding_changes(changes);
    let prepared = table
        .prepare_authored_source_publication_in(&tree, source, target, &[ordinary], &[], &[owner])
        .unwrap();
    let projection = table
        .prepared_promotion_projection(&tree, &prepared)
        .unwrap();
    let retained = projection
        .retained_bindings()
        .map(|entry| entry.map(|entry| entry.id))
        .collect::<Result<HashSet<_>, _>>()
        .unwrap();
    assert_eq!(retained, HashSet::from([selected, ordinary]));
    assert!(!retained.contains(&compiler_only));
    assert_eq!(
        projection
            .visible_changes()
            .map(|(name, _)| name.0.as_str())
            .collect::<Vec<_>>(),
        vec!["ordinary"]
    );
    // The native preflight must observe a missing selected entry explicitly;
    // it cannot turn an unavailable root into an empty successful inventory.
    table.drain_scope(source);
    let projection = table
        .prepared_promotion_projection(&tree, &prepared)
        .unwrap();
    let unavailable = projection.retained_bindings().collect::<Vec<_>>();
    assert_eq!(unavailable.len(), 2);
    assert!(unavailable
        .into_iter()
        .all(|entry| matches!(entry, Err(BindingPromotionError::MissingOrForeignBinding))));
    assert!(table.iter_current_in(&tree, target).is_empty());
}

#[test]
fn declaration_all_prospective_owners_require_same_issuance_even_for_equal_support() {
    for with_root in [false, true] {
        let mut tree = ScopeTree::new();
        let scope = tree.mint_child(ScopeId::ROOT).unwrap();
        let child = tree.mint_isolated();
        let mut table = BindingTable::new();
        let id = table.bind_in(scope, entry("root", 260)).unwrap();
        let roots = if with_root {
            table
                .promote_exact_bindings_in(scope, scope, &[id])
                .unwrap();
            vec![id]
        } else {
            Vec::new()
        };
        let owner = declaration_owner(15);
        let mut x = table
            .prepare_source_owner_origin_in(&tree, scope, owner.clone())
            .unwrap();
        let mut y = table
            .prepare_source_owner_origin_in(&tree, scope, owner)
            .unwrap();
        let dx = table
            .prepare_declaration_binding_changes_with_custody_in(
                &tree,
                scope,
                &mut x,
                &[],
                &[],
                &roots,
            )
            .unwrap();
        let dy = table
            .prepare_declaration_binding_changes_with_custody_in(
                &tree,
                scope,
                &mut y,
                &[],
                &[],
                &roots,
            )
            .unwrap();
        assert!(dx.custody.is_none() && dy.custody.is_none());
        assert_eq!(x.binding_support(), y.binding_support());
        assert!(!Arc::ptr_eq(
            x.binding_support().unwrap(),
            y.binding_support().unwrap()
        ));
        let before = table.scope_snapshot(&tree, scope).unwrap();
        let revision = table.revision;
        let leases = table.lease_count(id);
        let next_tip = table.next_tip;
        for (origin, changes) in [(&x, &dy), (&y, &dx)] {
            without_allocations(|| {
                assert!(matches!(
                    table.prepared_declaration_projection(&tree, Some(origin), changes),
                    Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
                ));
                assert!(matches!(
                    table.prepared_scope_snapshot_in(&tree, scope, Some(origin), Some(changes)),
                    Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
                ));
                assert!(matches!(
                    table.prepared_iter_current_in(&tree, scope, Some(origin), changes),
                    Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
                ));
                assert!(matches!(
                    table.prepare_seed_detached_scope_for_declaration(
                        &tree, scope, child, origin, changes
                    ),
                    Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
                ));
            });
        }
        without_allocations(|| {
            assert!(matches!(
                table.prepared_scope_snapshot_in(&tree, scope, None, Some(&dx)),
                Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
            ));
            assert!(matches!(
                table.prepared_scope_snapshot_in(&tree, scope, Some(&x), None),
                Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
            ));
            assert!(matches!(
                table.prepared_iter_current_in(&tree, scope, None, &dx),
                Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
            ));
        });
        assert_eq!(table.scope_snapshot(&tree, scope).unwrap(), before);
        assert_eq!(table.revision, revision);
        assert_eq!(table.lease_count(id), leases);
        assert_eq!(table.next_tip, next_tip);
        assert!(table.tip_id(child).is_none());
        // A correctly paired empty or already-promoted preparation remains
        // supported at every entry, including unseeded ancestor lookup.
        let expected = table
            .prepared_scope_snapshot_in(&tree, scope, Some(&x), Some(&dx))
            .unwrap();
        let expected_visible = table
            .prepared_iter_current_in(&tree, scope, Some(&x), &dx)
            .unwrap()
            .into_iter()
            .map(|(name, entry)| (name.0, entry.id))
            .collect::<Vec<_>>();
        let detached = table
            .prepare_seed_detached_scope_for_declaration(&tree, scope, child, &x, &dx)
            .unwrap();
        without_allocations(|| {
            table.commit_source_owner_origin(x);
            table.commit_declaration_binding_changes(dx);
            table.commit_prepared_detached_scope(detached);
        });
        assert_eq!(table.scope_snapshot(&tree, scope).unwrap(), expected);
        assert_eq!(visible(&table, &tree, child), expected_visible);
    }
}

#[test]
fn declaration_prospective_owners_refuse_foreign_or_stale_bare_preparations() {
    let mut tree = ScopeTree::new();
    let scope = tree.mint_isolated();
    let child = tree.mint_isolated();
    let mut table = BindingTable::new();
    let mut foreign = BindingTable::new();
    let origin = foreign
        .prepare_source_owner_origin_in(&tree, scope, declaration_owner(16))
        .unwrap();
    let changes = foreign
        .prepare_declaration_binding_changes_in(scope, &[], &[])
        .unwrap();
    without_allocations(|| {
        assert!(matches!(
            table.prepared_scope_snapshot_in(&tree, scope, Some(&origin), Some(&changes)),
            Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
        ));
        assert!(matches!(
            table.prepared_iter_current_in(&tree, scope, Some(&origin), &changes),
            Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
        ));
        assert!(matches!(
            table.prepare_seed_detached_scope_for_declaration(
                &tree, scope, child, &origin, &changes
            ),
            Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
        ));
    });
    let origin = table
        .prepare_source_owner_origin_in(&tree, scope, declaration_owner(16))
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_in(scope, &[], &[])
        .unwrap();
    table.bind_in(scope, entry("new", 261)).unwrap();
    without_allocations(|| {
        assert!(matches!(
            table.prepared_scope_snapshot_in(&tree, scope, Some(&origin), Some(&changes)),
            Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
        ));
        assert!(matches!(
            table.prepared_iter_current_in(&tree, scope, Some(&origin), &changes),
            Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
        ));
        assert!(matches!(
            table.prepare_seed_detached_scope_for_declaration(
                &tree, scope, child, &origin, &changes
            ),
            Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
        ));
    });
    assert!(table.tip_id(child).is_none());
    assert_eq!(table.lease_count(SessionVarId::from_extract(261)), 0);
}

#[test]
fn declaration_frozen_support_forks_exact_ids_and_excludes_unaccepted_owners() {
    fn check_domains(
        table: &BindingTable,
        tree: &ScopeTree,
        scope: ScopeId,
        owner: &tidepool_repr::execution_schema::CachedHomeOwner,
    ) {
        let native = table.native_domain_selection_in(tree, scope).unwrap();
        let home = table.source_domain_selection_in(tree, scope).unwrap();
        let view = &table.source_selection[&scope];
        assert_eq!(native.current(), view.current);
        for (caller, record) in view.domains.iter() {
            for (owner, origin) in record.authored.iter() {
                assert_eq!(
                    native.domain_for_owner(*caller, owner).unwrap(),
                    origin.domain
                );
                assert_eq!(
                    native.domain_for_owner(*caller, owner).unwrap(),
                    home.domain_for_owner(*caller, owner).unwrap()
                );
            }
            assert_eq!(
                native
                    .domain_for_owner(*caller, &declaration_owner(99))
                    .unwrap(),
                *caller
            );
        }
        assert_ne!(
            native.domain_for_owner(native.current(), owner).unwrap(),
            native.current()
        );
        // An unrelated scope is not an admitted caller domain in this view.
        let foreign = table
            .native_domain_selection_in(tree, ScopeId::ROOT)
            .unwrap()
            .current();
        assert!(
            matches!(native.domain_for_owner(foreign, owner), Err(crate::prepared_program::DemandError::MissingDomain(actual)) if actual == foreign)
        );
    }
    let mut tree = ScopeTree::new();
    let source = tree.mint_isolated();
    let public = tree.mint_isolated();
    let child = tree.mint_isolated();
    let later_public = tree.mint_isolated();
    let mut table = BindingTable::new();
    let selected = table.bind_in(source, entry("selected", 250)).unwrap();
    let unrelated = table.bind_in(source, entry("unrelated", 251)).unwrap();
    let owner = declaration_owner(11);
    let mut issued = None;
    for (owner, id) in [
        (owner.clone(), selected),
        (declaration_owner(12), unrelated),
    ] {
        let mut origin = table
            .prepare_source_owner_origin_in(&tree, source, owner)
            .unwrap();
        let changes = table
            .prepare_declaration_binding_changes_with_custody_in(
                &tree,
                source,
                &mut origin,
                &[],
                &[],
                &[id],
            )
            .unwrap();
        if id == selected {
            issued = origin.binding_support().cloned();
        }
        table.commit_source_owner_origin(origin);
        table.commit_declaration_binding_changes(changes);
    }
    let publication = table
        .prepare_authored_source_publication_in(&tree, source, public, &[], &[], &[owner.clone()])
        .unwrap();
    table.commit_exact_binding_promotion(publication);
    check_domains(&table, &tree, public, &owner);
    assert_eq!(table.lease_count(unrelated), 1);
    table.drain_scope(source);
    assert!(table.get(unrelated).is_none());
    assert!(table.get(selected).is_some());
    table.seed_scope(&tree, public, child);
    check_domains(&table, &tree, child, &owner);
    let view = &table.source_selection[&child];
    let inherited = &view.domains[&view.current].authored[&owner];
    assert!(Arc::ptr_eq(
        issued.as_ref().unwrap(),
        inherited.support.as_ref().unwrap()
    ));
    assert_ne!(
        inherited.domain,
        table.source_selection[&public].domains[&table.source_selection[&public].current].authored
            [&owner]
            .domain
    );
    table.drain_scope(public);
    let publication = table
        .prepare_authored_source_publication_in(
            &tree,
            child,
            later_public,
            &[],
            &[],
            &[owner.clone()],
        )
        .unwrap();
    table.commit_exact_binding_promotion(publication);
    table.drain_scope(child);
    check_domains(&table, &tree, later_public, &owner);
    assert_eq!(table.get(selected).unwrap().id, selected);
    assert_eq!(table.drain_scope(later_public).len(), 1);
    assert!(table.get(selected).is_none());
}

#[test]
fn declaration_missing_support_is_distinct_from_issued_empty_support() {
    let mut tree = ScopeTree::new();
    let source = tree.mint_isolated();
    let target = tree.mint_isolated();
    let mut table = BindingTable::new();
    let missing = declaration_owner(13);
    let origin = table
        .prepare_source_owner_origin_in(&tree, source, missing.clone())
        .unwrap();
    table.commit_source_owner_origin(origin);
    assert!(matches!(
        table.prepare_authored_source_publication_in(&tree, source, target, &[], &[], &[missing]),
        Err(BindingPromotionError::MissingAuthoredBindingSupport)
    ));
    let empty = declaration_owner(14);
    let mut origin = table
        .prepare_source_owner_origin_in(&tree, source, empty.clone())
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_with_custody_in(
            &tree,
            source,
            &mut origin,
            &[],
            &[],
            &[],
        )
        .unwrap();
    assert!(origin.binding_support().unwrap().roots.is_empty());
    assert!(table
        .prepared_declaration_projection(&tree, Some(&origin), &changes)
        .is_ok());
    table.commit_source_owner_origin(origin);
    table.commit_declaration_binding_changes(changes);
    let prepared = table
        .prepare_authored_source_publication_in(&tree, source, target, &[], &[], &[empty])
        .unwrap();
    without_allocations(|| table.commit_exact_binding_promotion(prepared));
    assert!(table.iter_current_in(&tree, target).is_empty());
}

#[test]
fn prepared_declaration_projection_matches_committed_observation_and_name_histories() {
    // Capture sets include target, ancestor and unrelated owners. A target's
    // remaining observer must see the global dependency revision change even
    // when the removed observation belongs to an unrelated scope.
    for seeded in [false, true] {
        for captured in 0u8..16 {
            for with_origin in [false, true] {
                let mut tree = ScopeTree::new();
                let target = tree.mint_child(ScopeId::ROOT).unwrap();
                let outside = tree.mint_isolated();
                let mut table = BindingTable::new();
                let ancestor = table.bind_in(ScopeId::ROOT, entry("answer", 1)).unwrap();
                table.save_observation(ancestor, &[], 100);
                if seeded {
                    table.seed_scope(&tree, ScopeId::ROOT, target);
                }
                let local = table.bind_in(target, entry("answer", 2)).unwrap();
                let alias = table
                    .bind_alias_in(target, entry("alias", 3), local)
                    .unwrap()
                    .0;
                let unrelated = table.bind_in(outside, entry("unrelated", 4)).unwrap();
                table.save_observation(local, &[], 100);
                table.save_observation(alias, &[local.var()], 100);
                table.save_observation(unrelated, &[], 100);
                let ids = [ancestor, local, alias, unrelated];
                let captures = ids
                    .iter()
                    .enumerate()
                    .filter(|(slot, _)| captured & (1 << slot) != 0)
                    .map(|(_, id)| id.var())
                    .collect::<Vec<_>>();
                // Repeated no-op, local removal/ancestor fallback, alias
                // withdrawal, already-hidden name, and a different origin.
                for (step, removals) in [
                    vec!["missing", "missing"],
                    vec!["answer", "alias", "answer"],
                    vec!["answer"],
                    vec![],
                ]
                .into_iter()
                .enumerate()
                {
                    let removals = removals.into_iter().map(str::to_owned).collect::<Vec<_>>();
                    let before = table.scope_snapshot(&tree, target).unwrap();
                    let before_witness = table.scope_witness(&tree, target).unwrap();
                    let before_revision = table.mutation_revision();
                    let origin = with_origin.then(|| {
                        table
                            .prepare_source_owner_origin_in(
                                &tree,
                                target,
                                declaration_owner(step as u8),
                            )
                            .unwrap()
                    });
                    let changes = table
                        .prepare_declaration_binding_changes_in(target, &captures, &removals)
                        .unwrap();
                    let projection = table
                        .prepared_declaration_projection(&tree, origin.as_ref(), &changes)
                        .unwrap();
                    assert_eq!(projection.target_scope(), target);
                    let predicted = projection.scope_witness().clone();
                    let selections = projection
                        .visible_changes()
                        .map(|(name, entry)| {
                            (name.0.clone(), entry.map(|entry| (entry.id, entry.scope)))
                        })
                        .collect::<BTreeMap<_, _>>();
                    assert_eq!(
                        selections.keys().cloned().collect::<BTreeSet<_>>(),
                        removals.iter().cloned().collect()
                    );
                    assert_eq!(table.scope_snapshot(&tree, target).unwrap(), before);
                    assert_eq!(table.scope_witness(&tree, target).unwrap(), before_witness);
                    assert_eq!(table.mutation_revision(), before_revision);
                    without_allocations(|| {
                        if let Some(origin) = origin {
                            table.commit_source_owner_origin(origin);
                        }
                        table.commit_declaration_binding_changes(changes);
                    });
                    assert_eq!(table.scope_witness(&tree, target), Some(predicted));
                    for (name, selected) in selections {
                        assert_eq!(
                            table
                                .resolve_in(&tree, target, &name)
                                .map(|entry| (entry.id, entry.scope)),
                            selected
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn prepared_declaration_projection_refuses_foreign_stale_and_retired_before_mutation() {
    let mut tree = ScopeTree::new();
    let scope = tree.mint_isolated();
    let other_scope = tree.mint_isolated();
    let mut table = BindingTable::new();
    let mut foreign = BindingTable::new();
    let changes = table
        .prepare_declaration_binding_changes_in(scope, &[], &[])
        .unwrap();
    let foreign_changes = foreign
        .prepare_declaration_binding_changes_in(scope, &[], &[])
        .unwrap();
    let foreign_origin = foreign
        .prepare_source_owner_origin_in(&tree, scope, declaration_owner(0))
        .unwrap();
    let wrong_scope_origin = table
        .prepare_source_owner_origin_in(&tree, other_scope, declaration_owner(0))
        .unwrap();
    assert_eq!(table.revision, foreign.revision);
    for (origin, delta) in [
        (None, &foreign_changes),
        (Some(&foreign_origin), &changes),
        (Some(&wrong_scope_origin), &changes),
    ] {
        without_allocations(|| {
            assert!(matches!(
                table.prepared_declaration_projection(&tree, origin, delta),
                Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
            ))
        });
    }
    let before = table.scope_snapshot(&tree, scope).unwrap();
    tree.retire(scope);
    assert!(matches!(
        table.prepared_declaration_projection(&tree, None, &changes),
        Err(BindingPromotionError::UnverifiableSourceSelection)
    ));
    assert_eq!(table.revision, 1);
    assert!(table.live.is_empty());
    let origin = table
        .prepare_source_owner_origin_in(&tree, other_scope, declaration_owner(0))
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_in(other_scope, &[], &[])
        .unwrap();
    table.bind_in(other_scope, entry("changed", 1)).unwrap();
    let current = table.scope_snapshot(&tree, other_scope).unwrap();
    without_allocations(|| {
        assert!(matches!(
            table.prepared_declaration_projection(&tree, Some(&origin), &changes),
            Err(BindingPromotionError::StaleOrForeignDeclarationPreparation)
        ))
    });
    assert_eq!(table.scope_snapshot(&tree, other_scope).unwrap(), current);
    // The refusal did not publish either prepared origin.
    assert_eq!(table.source_selection.get(&scope), None);
    assert!(before.observation_dependencies.is_empty());
}

#[test]
fn prepared_declaration_projection_refuses_revision_exhaustion_before_commit() {
    for revision in [0, u64::MAX - 2, u64::MAX - 1, u64::MAX] {
        let tree = ScopeTree::new();
        let scope = ScopeId::ROOT;
        let mut table = BindingTable::new();
        let id = table.bind_in(scope, entry("value", 1)).unwrap();
        table.save_observation(id, &[], 10);
        let mut origin = table
            .prepare_source_owner_origin_in(&tree, scope, declaration_owner(0))
            .unwrap();
        table.revision = revision;
        // Set the opaque baseline at the tested exhaustion boundary; genuine
        // origin preparation already refuses a previously exhausted table.
        origin.baseline.revision = revision;
        let changes = table
            .prepare_declaration_binding_changes_in(scope, &[id.var()], &["value".into()])
            .unwrap();
        let before = table.scope_witness(&tree, scope);
        assert!(
            matches!(table.prepared_declaration_projection(&tree, Some(&origin), &changes),
            Err(BindingPromotionError::MutationWitnessExhausted { revision: observed, advances: 3 }) if observed == revision)
        );
        assert_eq!(table.scope_witness(&tree, scope), before);
        assert_eq!(table.resolve_in(&tree, scope, "value").unwrap().id, id);
        assert!(table.observations.contains_key(&id));
        assert_eq!(table.revision, revision);
    }
    let tree = ScopeTree::new();
    let mut table = BindingTable::new();
    table.revision = u64::MAX - 1;
    let origin = table
        .prepare_source_owner_origin_in(&tree, ScopeId::ROOT, declaration_owner(0))
        .unwrap();
    let changes = table
        .prepare_declaration_binding_changes_in(ScopeId::ROOT, &[], &[])
        .unwrap();
    let predicted = table
        .prepared_declaration_projection(&tree, Some(&origin), &changes)
        .unwrap()
        .scope_witness()
        .clone();
    without_allocations(|| {
        table.commit_source_owner_origin(origin);
        table.commit_declaration_binding_changes(changes);
    });
    assert_eq!(table.scope_witness(&tree, ScopeId::ROOT), Some(predicted));
    assert_eq!(table.revision, u64::MAX);
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
        .prepared_iter_current_in(&tree, child, Some(&origin), &changes)
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

#[test]
fn exact_visibility_query_tracks_frozen_and_promoted_identities() {
    let mut tree = ScopeTree::new();
    let owner = tree.mint_isolated();
    let frozen = tree.mint_isolated();
    let public = tree.mint_isolated();
    let sibling = tree.mint_isolated();
    let mut table = BindingTable::new();
    let original = table.bind_in(owner, entry("x", 701)).unwrap();
    table.seed_scope(&tree, owner, frozen);
    table
        .promote_exact_bindings_in(owner, public, &[original])
        .unwrap();
    let other = table.bind_in(sibling, entry("x", 702)).unwrap();
    assert_eq!(
        table.is_binding_visible_in(&tree, frozen, original),
        Ok(true)
    );
    assert_eq!(
        table.is_binding_visible_in(&tree, public, original),
        Ok(true)
    );
    assert_eq!(
        table.is_binding_visible_in(&tree, sibling, original),
        Ok(false)
    );
    assert_eq!(table.is_binding_visible_in(&tree, sibling, other), Ok(true));
    let alias_entry = entry("alias", 703);
    let alias = alias_entry.id;
    table.bind_alias_in(owner, alias_entry, original).unwrap();
    table.remove_current_in(owner, "x");
    assert_eq!(
        table.is_binding_visible_in(&tree, owner, original),
        Ok(false)
    );
    assert_eq!(table.is_binding_visible_in(&tree, owner, alias), Ok(true));
    assert_eq!(
        table.is_binding_visible_in(&tree, frozen, original),
        Ok(true)
    );
    assert_eq!(
        table.is_binding_visible_in(&tree, public, original),
        Ok(true)
    );
    let changed_name = entry("forged_alias", 701);
    assert!(table.bind_in(owner, changed_name).is_err());
    assert_eq!(
        table.is_binding_visible_in(&tree, sibling, original),
        Ok(false)
    );
}

#[test]
fn prepared_projection_preserves_ancestor_after_local_removal() {
    let mut tree = ScopeTree::new();
    let target = tree.mint_child(ScopeId::ROOT).unwrap();
    let source = tree.mint_isolated();
    let mut table = BindingTable::new();
    let ancestor = table.bind_in(ScopeId::ROOT, entry("ancestor", 7)).unwrap();
    let token = table
        .prepare_exact_binding_promotion_in(source, target, &[])
        .unwrap();
    let token = table
        .prepare_binding_publication_removals(token, &["ancestor".into()])
        .unwrap();
    let projection = table.prepared_promotion_projection(&tree, &token).unwrap();
    let changes = projection
        .visible_changes()
        .map(|(name, entry)| (name.0.clone(), entry.map(|entry| entry.id)))
        .collect::<Vec<_>>();
    assert_eq!(changes, vec![("ancestor".into(), Some(ancestor))]);
    without_allocations(|| table.commit_exact_binding_promotion(token));
    assert_eq!(
        table.resolve_in(&tree, target, "ancestor").unwrap().id,
        ancestor
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn prepared_promotion_witness_matches_committed_history(
        seeded in any::<bool>(),
        history in prop::collection::vec((0u8..16, 0u8..16, any::<bool>()), 0..32),
    ) {
        let mut tree = ScopeTree::new();
        let target = tree.mint_child(ScopeId::ROOT).unwrap();
        let mut table = BindingTable::new();
        let ancestor = table.bind_in(ScopeId::ROOT, entry("ancestor", 7)).unwrap();
        table.save_observation(ancestor, &[], 8);
        if seeded {
            table.seed_scope(&tree, ScopeId::ROOT, target);
        }
        // Every history covers append, shadow, several names, removals and
        // an empty name delta; the suffix shrinks independently.
        let prefix = [(1, 0, true), (1, 0, true), (6, 1, true), (0, 0, true), (0, 12, true)];
        let names = ["name0", "name1", "name2", "ancestor"];
        for (step, (writes, removals, commit)) in prefix.into_iter().chain(history).enumerate() {
            let source = tree.mint_isolated();
            let mut ids = Vec::new();
            for (slot, name) in names.iter().enumerate() {
                if writes & (1u8 << slot) != 0 {
                    ids.push(table.bind_in(source, entry(name, 1000 + (step * 4 + slot) as u64)).unwrap());
                }
            }
            let removals = names.iter().enumerate()
                .filter(|(slot, _)| removals & (1u8 << *slot) != 0)
                .map(|(_, name)| (*name).to_owned())
                .collect::<Vec<_>>();
            let before = table.scope_witness(&tree, target).unwrap();
            let token = table.prepare_exact_binding_promotion_in(source, target, &ids).unwrap();
            let token = table.prepare_binding_publication_removals(token, &removals).unwrap();
            let predicted = table.prepared_promotion_scope_witness(&tree, &token).unwrap();
            let projection = table.prepared_promotion_projection(&tree, &token).unwrap();
            prop_assert_eq!(projection.target_scope(), target);
            let projected_changes = projection.visible_changes()
                .map(|(name, entry)| (name.0.clone(), entry.map(|entry| entry.id)))
                .collect::<BTreeMap<_, _>>();
            let expected_touched = ids.iter().map(|id| table.get(*id).unwrap().name.0.clone())
                .chain(removals.iter().cloned()).collect::<BTreeSet<_>>();
            prop_assert_eq!(projected_changes.keys().cloned().collect::<BTreeSet<_>>(), expected_touched);
            let mut expected_visible = visible(&table, &tree, target).into_iter().collect::<BTreeMap<_, _>>();
            for (name, entry) in projected_changes {
                if let Some(id) = entry { expected_visible.insert(name, id); }
                else { expected_visible.remove(&name); }
            }
            prop_assert_eq!(table.scope_witness(&tree, target), Some(before.clone()));
            if commit {
                without_allocations(|| table.commit_exact_binding_promotion(token));
                prop_assert_eq!(table.scope_witness(&tree, target), Some(predicted));
                prop_assert_eq!(visible(&table, &tree, target).into_iter().collect::<BTreeMap<_, _>>(), expected_visible);
            } else {
                drop(token);
                prop_assert_eq!(table.scope_witness(&tree, target), Some(before));
            }
        }
    }

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
fn prepared_promotion_witness_refuses_exhaustion_before_mutation() {
    for revision in [0, u64::MAX - 1] {
        let mut tree = ScopeTree::new();
        let source = tree.mint_isolated();
        let target = tree.mint_isolated();
        let mut table = BindingTable::new();
        let first = table.bind_in(source, entry("first", 1)).unwrap();
        let second = table.bind_in(source, entry("second", 2)).unwrap();
        table.revision = revision;
        let token = table
            .prepare_exact_binding_promotion_in(source, target, &[first, second])
            .unwrap();
        let token = table
            .prepare_binding_publication_removals(token, &["first".to_owned()])
            .unwrap();
        let before = table.scope_witness(&tree, target);
        let before_visible = visible(&table, &tree, target);
        assert!(matches!(
            table.prepared_promotion_scope_witness(&tree, &token),
            Err(BindingPromotionError::MutationWitnessExhausted { revision: observed, advances })
                if observed == revision && advances == 2
        ));
        assert_eq!(table.scope_witness(&tree, target), before);
        assert_eq!(visible(&table, &tree, target), before_visible);
        assert_eq!(table.revision, revision);
    }

    let mut tree = ScopeTree::new();
    let source = tree.mint_isolated();
    let target = tree.mint_isolated();
    let mut table = BindingTable::new();
    let id = table.bind_in(source, entry("last", 3)).unwrap();
    table.revision = u64::MAX - 1;
    let token = table
        .prepare_exact_binding_promotion_in(source, target, &[id])
        .unwrap();
    let predicted = table
        .prepared_promotion_scope_witness(&tree, &token)
        .unwrap();
    without_allocations(|| table.commit_exact_binding_promotion(token));
    assert_eq!(table.mutation_revision(), Some(u64::MAX));
    assert_eq!(table.scope_witness(&tree, target), Some(predicted));
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
                let prospective: Vec<_> = table.prepared_iter_current_in(&tree, parent, Some(&origin), &changes).unwrap().into_iter()
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

#[test]
fn capture_counter_preflight_refuses_exhaustion_without_mutation() {
    let tree = ScopeTree::new();
    let scope = ScopeId::ROOT;
    let mut table = BindingTable::new();
    for generation in 1..=10 {
        table.validate_capture_capacity(1, true).unwrap();
        let id = table
            .bind_in(scope, entry(&format!("capture{generation}"), generation))
            .unwrap();
        table.save_observation(id, &[], 8);
    }
    assert!(!visible(&table, &tree, scope)
        .iter()
        .any(|(name, _)| name == "capture1"));
    assert!(visible(&table, &tree, scope)
        .iter()
        .any(|(name, _)| name == "capture10"));
    let before = visible(&table, &tree, scope);
    table.next_observation_order = u64::MAX;
    assert_eq!(
        table.validate_capture_capacity(1, true),
        Err(BindingPromotionError::ObservationOrderExhausted { order: u64::MAX })
    );
    assert_eq!(visible(&table, &tree, scope), before);
    table.next_observation_order = 11;
    table.revision = u64::MAX;
    assert!(matches!(
        table.validate_capture_capacity(1, false),
        Err(BindingPromotionError::MutationWitnessExhausted {
            revision: u64::MAX,
            ..
        })
    ));
    assert_eq!(visible(&table, &tree, scope), before);
}
