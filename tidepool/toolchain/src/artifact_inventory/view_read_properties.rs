//! Read projections are checked against a full graph fixed point and then
//! exercised repeatedly to ensure a warmed immutable view does not traverse
//! the inventory graph again.
use super::*;
use proptest::prelude::*;
use proptest::test_runner::{FileFailurePersistence, TestCaseError};
use std::sync::{mpsc, Barrier};
use std::time::Duration;

const OWNERS: usize = 5;

struct Catalog {
    entries: Vec<ArtifactEntry>,
}

impl Catalog {
    fn new(masks: &[u8; OWNERS], seed: u8) -> Self {
        let mut entries = (0..OWNERS)
            .map(|key| {
                let unit = format!("unit{}", key % 2);
                let module = format!("Read{key}");
                let requirements = (0..OWNERS)
                    .filter(|target| target != &key && masks[key] & (1 << target) != 0)
                    .map(|target| {
                        let owner = ExactModuleIdentity {
                            unit: format!("unit{}", target % 2),
                            module: format!("Read{target}"),
                        };
                        ((owner.unit, owner.module.clone()), digest(owner.module.as_bytes()))
                    })
                    .collect();
                ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                    [seed; 32], &unit, &module, requirements,
                ))
            })
            .collect::<Vec<_>>();
        let product = crate::certified_products::tests::original_groups_fixture(
            &format!("ReadNative{seed}"),
            vec![(3, Vec::new()), (17, Vec::new())],
            1,
            &BTreeMap::new(),
        );
        entries.push(
            ArtifactEntry::original(
                [2; 32],
                crate::certified_products::fixture_finalized_product_with_requirements(
                    product,
                    [2; 32],
                    None,
                ),
            )
            .expect("native fixture is finalized"),
        );
        entries.push(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                [seed; 32],
                "unit_after_drop",
                "ReadAfterDrop",
                BTreeMap::new(),
            ),
        ));
        Self { entries }
    }

    fn owner(&self, index: usize) -> ExactModuleIdentity {
        self.entries[index].descriptor.owner.clone()
    }
}

#[derive(Default)]
struct Oracle {
    roots: BTreeSet<InventoryNodeKey>,
    closure: BTreeSet<InventoryNodeKey>,
    entries: Vec<Arc<ArtifactEntry>>,
    dependencies: Vec<(ArtifactId, ArtifactId, ArtifactDependency)>,
}

/// This intentionally scans every graph node and repeatedly expands edge
/// facts. It does not call the production closure, index, or cached projection.
fn exhaustive_oracle(view: &ArtifactView) -> Oracle {
    let mut result = Oracle::default();
    let mut pending = vec![view];
    let mut seen_views = BTreeSet::new();
    while let Some(view) = pending.pop() {
        let address = Arc::as_ptr(&view.0) as usize;
        if !seen_views.insert(address) {
            continue;
        }
        result.roots.extend(view.0.roots.iter().copied());
        pending.extend(view.0.parents.iter());
    }

    let state = view.0.inventory.0.lock().expect("inventory lock");
    let mut adjacency = BTreeMap::<InventoryNodeKey, BTreeSet<InventoryNodeKey>>::new();
    let mut edge_facts = Vec::new();
    for source in state.graph.node_indices() {
        let key = state.graph[source];
        adjacency.entry(key).or_default();
        for edge in state.graph.edges(source) {
            let target = state.graph[edge.target()];
            adjacency.entry(key).or_default().insert(target);
            edge_facts.push((key, target, edge.weight().clone()));
        }
    }
    result.closure = result
        .roots
        .iter()
        .filter(|root| adjacency.contains_key(root))
        .copied()
        .collect();
    loop {
        let before = result.closure.len();
        let targets = result
            .closure
            .iter()
            .flat_map(|node| adjacency.get(node).into_iter().flatten().copied())
            .collect::<Vec<_>>();
        result.closure.extend(targets);
        if result.closure.len() == before {
            break;
        }
    }
    let artifact_ids = result
        .closure
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Artifact(id) => Some(*id),
            InventoryNodeKey::Group(_) => None,
        })
        .collect::<BTreeSet<_>>();
    result.entries = artifact_ids
        .iter()
        .map(|id| Arc::clone(&state.payloads[id]))
        .collect();
    result.entries.sort_by(|left, right| {
        (&left.descriptor.owner, left.descriptor.id)
            .cmp(&(&right.descriptor.owner, right.descriptor.id))
    });
    result.dependencies = edge_facts
        .into_iter()
        .filter_map(|(source, target, edge)| {
            if !result.closure.contains(&source) || !result.closure.contains(&target) {
                return None;
            }
            // The group-to-carrier edge only represents graph custody.
            if matches!(source, InventoryNodeKey::Group(group) if target == InventoryNodeKey::Artifact(group.artifact))
                && matches!(&edge, ArtifactDependency::Interface)
            {
                None
            } else {
                Some((source.artifact(), target.artifact(), edge))
            }
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    result
}

fn check_read_projection(view: &ArtifactView) -> Result<(), TestCaseError> {
    let expected = exhaustive_oracle(view);
    let expected_ids = expected
        .entries
        .iter()
        .map(|entry| entry.descriptor.id)
        .collect::<Vec<_>>();
    let expected_id_set = expected_ids.iter().copied().collect::<BTreeSet<_>>();
    let expected_descriptors = expected
        .entries
        .iter()
        .map(|entry| entry.descriptor.clone())
        .collect::<Vec<_>>();
    let expected_groups = expected
        .closure
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Group(group) => Some(*group),
            InventoryNodeKey::Artifact(_) => None,
        })
        .collect::<BTreeSet<_>>();
    let expected_roots = expected
        .roots
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Artifact(id) => Some(*id),
            InventoryNodeKey::Group(_) => None,
        })
        .collect::<BTreeSet<_>>();
    let mut expected_metadata_entries = BTreeMap::new();
    let mut expected_interfaces = BTreeMap::new();
    let mut expected_natives = BTreeMap::<ExactModuleIdentity, Vec<Arc<ArtifactEntry>>>::new();
    // Metadata resolution gives an exact native implementation precedence over
    // an interface with the same owner; never derive the winner from IDs.
    for entry in &expected.entries {
        if !entry.is_native() {
            expected_interfaces.insert(entry.descriptor.owner.clone(), Arc::clone(entry));
            expected_metadata_entries
                .insert(entry.descriptor.owner.clone(), Arc::clone(entry));
        } else {
            expected_natives
                .entry(entry.descriptor.owner.clone())
                .or_default()
                .push(Arc::clone(entry));
        }
    }
    for (owner, native_entries) in expected_natives {
        prop_assert_eq!(native_entries.len(), 1, "fixture keeps native owners unique");
        expected_metadata_entries.insert(owner, Arc::clone(&native_entries[0]));
    }
    let mut expected_owners = expected_interfaces
        .values()
        .map(|entry| ExactInterfaceOwner {
            owner: entry.descriptor.owner.clone(),
            requirements: entry.requirements.clone(),
        })
        .collect::<Vec<_>>();
    expected_owners.sort_by(|left, right| left.owner.cmp(&right.owner));

    let metadata = view.metadata_snapshot();
    let mut descriptors = view.descriptors();
    descriptors.sort_by_key(|descriptor| (descriptor.owner.clone(), descriptor.id));
    let mut expected_descriptors = expected_descriptors;
    expected_descriptors.sort_by_key(|descriptor| (descriptor.owner.clone(), descriptor.id));
    let root_ids = view
        .root_entries()
        .into_iter()
        .map(|entry| entry.descriptor.id)
        .collect::<BTreeSet<_>>();
    prop_assert_eq!(view.artifact_ids().into_iter().collect::<BTreeSet<_>>(), expected_id_set.clone());
    prop_assert_eq!(descriptors, expected_descriptors);
    prop_assert_eq!(root_ids, expected_roots);
    prop_assert_eq!(view.dependencies(), expected.dependencies);
    prop_assert_eq!(view.selected_native_groups(), expected_groups);
    prop_assert_eq!(view.interface_owners(), expected_owners);
    prop_assert_eq!(metadata.dependencies(), exhaustive_oracle(view).dependencies);
    prop_assert_eq!(metadata.selected_native_groups, expected_groups);
    prop_assert!(metadata.ambiguous_native_owners.is_empty());
    prop_assert_eq!(metadata.artifacts.keys().copied().collect::<BTreeSet<_>>(), expected_id_set);
    prop_assert_eq!(metadata.entries, expected_metadata_entries);
    prop_assert!(root_ids.len() <= expected.closure.len());
    Ok(())
}

fn property_config() -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 64;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

proptest! {
    #![proptest_config(property_config())]

    #[test]
    fn cached_read_projection_matches_exhaustive_graph_after_view_histories(
        left_masks in any::<[u8; OWNERS]>(),
        seed in any::<u8>(),
        choices in proptest::collection::vec(0usize..OWNERS, 0..12),
    ) {
        let mut masks = left_masks;
        for mask in &mut masks[..OWNERS - 1] {
            *mask &= !(1 << (OWNERS - 1));
        }
        masks[OWNERS - 1] = 0;
        let catalog = Catalog::new(&masks, seed);
        let other_catalog = Catalog::new(&masks, seed);
        let left = ArtifactInventory::default();
        let right = ArtifactInventory::default();
        let empty = left.empty_view();
        let all = left.admit(&empty, catalog.entries[..OWNERS].to_vec()).unwrap();
        let peer = all.select_roots(vec![catalog.entries[0].descriptor.id]).unwrap();
        check_read_projection(&all)?;
        check_read_projection(&peer)?;

        // Add a new root after both projections are warm. Existing immutable
        // views must keep their old reachable set while the extension sees it.
        let extended = left.admit(&all, vec![catalog.entries[OWNERS].clone()]).unwrap();
        check_read_projection(&all)?;
        check_read_projection(&peer)?;
        check_read_projection(&extended)?;

        let selected_ids = choices.iter().map(|index| catalog.entries[*index].descriptor.id).collect::<BTreeSet<_>>();
        let mut selected = all.select_roots(selected_ids.iter().copied().collect()).unwrap();
        check_read_projection(&selected)?;
        if let Some(index) = choices.first() {
            let projected = selected.interface_projection(&[catalog.owner(*index)]).unwrap();
            check_read_projection(&projected)?;
            prop_assert!(projected.selected_native_groups().is_empty());
            selected = selected.merge(&projected).unwrap();
            check_read_projection(&selected)?;
        }

        // Exercise parent-linked same-inventory merging and cross-inventory
        // admission of identical content IDs from an independent graph.
        let same_merged = peer.merge(&extended).unwrap();
        check_read_projection(&same_merged)?;
        let other = right.admit(&right.empty_view(), other_catalog.entries[..OWNERS].to_vec()).unwrap();
        let cross_merged = all.merge(&other).unwrap();
        check_read_projection(&cross_merged)?;

        // Release the full roots while a selected peer remains. Its read cache
        // must still describe the exact closure through holes in the graph.
        // A metadata snapshot owns copied Arc payload handles only. It must not
        // keep the graph alive or carry native selection into an interface view.
        let snapshot = extended.metadata_snapshot();
        prop_assert!(!extended.selected_native_groups().is_empty());
        let interface_owner = catalog.owner(0);
        let interface = extended.interface_projection(&[interface_owner]).unwrap();
        prop_assert!(interface.selected_native_groups().is_empty());
        prop_assert!(interface.metadata_snapshot().selected_native_groups.is_empty());
        prop_assert!(interface.0.parents.is_empty());
        prop_assert!(interface.0.materialization_parents.is_empty());
        prop_assert!(interface.0.materialization.lock().unwrap().is_empty());
        prop_assert!(interface.entries().iter().all(|entry| !entry.is_native()));
        let initial_indices = left
            .0
            .lock()
            .unwrap()
            .graph
            .node_indices()
            .collect::<BTreeSet<_>>();
        drop(all);
        drop(extended);
        drop(same_merged);
        drop(selected);
        drop(cross_merged);
        drop(other);
        check_read_projection(&peer)?;
        prop_assert_eq!(left.node_count(), exhaustive_oracle(&peer).closure.len());
        let remaining_indices = left
            .0
            .lock()
            .unwrap()
            .graph
            .node_indices()
            .collect::<BTreeSet<_>>();
        let removed_indices = initial_indices
            .difference(&remaining_indices)
            .copied()
            .collect::<BTreeSet<_>>();
        prop_assert!(!removed_indices.is_empty(), "history must reclaim graph nodes");
        let reused = left
            .admit(&peer, vec![catalog.entries[OWNERS + 1].clone()])
            .unwrap();
        let reused_index = left
            .0
            .lock()
            .unwrap()
            .indices[&InventoryNodeKey::Artifact(catalog.entries[OWNERS + 1].descriptor.id)];
        prop_assert!(removed_indices.contains(&reused_index), "new admission should reuse a reclaimed graph slot");
        check_read_projection(&peer)?;
        check_read_projection(&reused)?;
        prop_assert_eq!(left.node_count(), exhaustive_oracle(&peer).closure.len() + 1);
        drop(reused);
        drop(peer);
        drop(interface);
        prop_assert_eq!(left.node_count(), 0);
        prop_assert!(!snapshot.selected_native_groups.is_empty());
    }
}

#[test]
fn warmed_read_getters_do_not_visit_graph_nodes() {
    let masks = [0b00110, 0b00101, 0b00001, 0b00000, 0b00000];
    let catalog = Catalog::new(&masks, 91);
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit(&inventory.empty_view(), catalog.entries.clone())
        .unwrap();
    // Warm the shared reachable-set projection and lazy metadata first.
    let _ = view.metadata_snapshot();
    let _ = view.entries();
    let _ = view.dependencies();
    let before = inventory.metrics().graph_visits;
    let dependency_before = inventory
        .0
        .lock()
        .unwrap()
        .dependency_graph_visits
        .load(Ordering::Relaxed);
    let root_walk_before = view.0.root_parent_visits.load(Ordering::Relaxed);
    for _ in 0..12 {
        let _ = view.entries();
        let _ = view.descriptors();
        let _ = view.artifact_ids();
        let _ = view.root_entries();
        let _ = view.dependencies();
        let _ = view.selected_native_groups();
        let _ = view.interface_owners();
        let _ = view.metadata_snapshot();
    }
    assert_eq!(inventory.metrics().graph_visits, before);
    assert_eq!(
        inventory
            .0
            .lock()
            .unwrap()
            .dependency_graph_visits
            .load(Ordering::Relaxed),
        dependency_before
    );
    assert_eq!(
        view.0.root_parent_visits.load(Ordering::Relaxed),
        root_walk_before
    );
    check_read_projection(&view).unwrap();
}

#[test]
fn first_read_and_inventory_extension_settle_without_lock_recursion() {
    let catalog = Catalog::new(&[0b00010, 0b00001, 0, 0, 0], 37);
    let inventory = Arc::new(ArtifactInventory::default());
    let empty = inventory.empty_view();
    let view = Arc::new(
        inventory
            .admit(&empty, catalog.entries[..OWNERS].to_vec())
            .unwrap(),
    );
    let gate = Arc::new(Barrier::new(3));
    let (send, receive) = mpsc::channel::<&'static str>();

    let read_gate = Arc::clone(&gate);
    let read_view = Arc::clone(&view);
    let read_send = send.clone();
    let reader = std::thread::spawn(move || {
        read_gate.wait();
        let _ = read_view.metadata_snapshot().dependencies();
        let _ = read_send.send("read");
    });

    let admit_gate = Arc::clone(&gate);
    let admit_inventory = Arc::clone(&inventory);
    let admit_view = Arc::clone(&view);
    let added = catalog.entries[OWNERS].clone();
    let admission = std::thread::spawn(move || {
        admit_gate.wait();
        let extended = admit_inventory.admit(&admit_view, vec![added]).unwrap();
        let _ = extended.artifact_ids();
        let _ = send.send("admit");
    });

    gate.wait();
    let first = receive.recv_timeout(Duration::from_secs(5));
    let second = receive.recv_timeout(Duration::from_secs(5));
    assert!(first.is_ok() && second.is_ok(), "first read and admission must settle");
    assert_eq!(BTreeSet::from([first.unwrap(), second.unwrap()]), BTreeSet::from(["read", "admit"]));
    reader.join().unwrap();
    admission.join().unwrap();
    check_read_projection(&view).unwrap();
}
