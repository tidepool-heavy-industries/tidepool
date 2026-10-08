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
                        (
                            (owner.unit, owner.module.clone()),
                            digest(owner.module.as_bytes()),
                        )
                    })
                    .collect();
                ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                    [seed; 32],
                    &unit,
                    &module,
                    requirements,
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
                    product, [2; 32], None,
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
            expected_metadata_entries.insert(entry.descriptor.owner.clone(), Arc::clone(entry));
        } else {
            expected_natives
                .entry(entry.descriptor.owner.clone())
                .or_default()
                .push(Arc::clone(entry));
        }
    }
    for (owner, native_entries) in expected_natives {
        prop_assert_eq!(
            native_entries.len(),
            1,
            "fixture keeps native owners unique"
        );
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
    prop_assert_eq!(
        &view.artifact_ids().into_iter().collect::<BTreeSet<_>>(),
        &expected_id_set
    );
    prop_assert_eq!(&descriptors, &expected_descriptors);
    prop_assert_eq!(&root_ids, &expected_roots);
    prop_assert_eq!(&view.dependencies(), &expected.dependencies);
    prop_assert_eq!(&view.selected_native_groups(), &expected_groups);
    prop_assert_eq!(&view.interface_owners(), &expected_owners);
    prop_assert_eq!(
        &metadata.dependencies(),
        &exhaustive_oracle(view).dependencies
    );
    prop_assert_eq!(&metadata.selected_native_groups, &expected_groups);
    prop_assert!(metadata.ambiguous_native_owners.is_empty());
    prop_assert_eq!(
        &metadata.artifacts.keys().copied().collect::<BTreeSet<_>>(),
        &expected_id_set
    );
    prop_assert_eq!(&metadata.entries, &expected_metadata_entries);
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
            prop_assert!(
                projected.selected_native_groups().is_empty(),
                "interface projection carries no native groups"
            );
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
        prop_assert!(
            !extended.selected_native_groups().is_empty(),
            "extended native source has selected groups"
        );
        let interface_owner = catalog.owner(OWNERS);
        let extended_entries = extended.entries();
        let has_native_owner = extended_entries
            .iter()
            .any(|entry| entry.is_native() && entry.descriptor.owner == interface_owner);
        prop_assert!(has_native_owner, "extended view retains the native carrier");
        let has_interface_owner = extended_entries
            .iter()
            .any(|entry| !entry.is_native() && entry.descriptor.owner == interface_owner);
        prop_assert!(has_interface_owner, "extended view retains its interface");
        let interface = extended
            .interface_projection(&[interface_owner.clone()])
            .unwrap();
        prop_assert!(
            interface.selected_native_groups().is_empty(),
            "projected view has no selected native groups"
        );
        prop_assert!(
            interface.metadata_snapshot().selected_native_groups.is_empty(),
            "projected metadata has no selected native groups"
        );
        prop_assert!(interface.0.parents.is_empty(), "projection has no view parents");
        prop_assert!(
            interface.0.materialization_parents.is_empty(),
            "projection has no materialization parents"
        );
        prop_assert!(
            interface.0.materialization.lock().unwrap().is_empty(),
            "projection has no retained materializations"
        );
        let projected_entries = interface.entries();
        let projected_entries_are_interfaces =
            projected_entries.iter().all(|entry| !entry.is_native());
        prop_assert!(
            projected_entries_are_interfaces,
            "projection has no native entries"
        );
        let retains_interface = projected_entries
            .iter()
            .any(|entry| entry.descriptor.owner == interface_owner);
        prop_assert!(retains_interface, "projection retains the selected interface");
        drop(interface);
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
        prop_assert_eq!(
            &left.node_count(),
            &exhaustive_oracle(&peer).closure.len()
        );
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
        prop_assert_eq!(
            &left.node_count(),
            &(exhaustive_oracle(&peer).closure.len() + 1)
        );
        drop(reused);
        drop(peer);
        prop_assert_eq!(&left.node_count(), &0);
        prop_assert!(
            !snapshot.selected_native_groups.is_empty(),
            "detached snapshot preserves its copied native selection"
        );
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
    assert!(
        first.is_ok() && second.is_ok(),
        "first read and admission must settle"
    );
    assert_eq!(
        BTreeSet::from([first.unwrap(), second.unwrap()]),
        BTreeSet::from(["read", "admit"])
    );
    reader.join().unwrap();
    admission.join().unwrap();
    check_read_projection(&view).unwrap();
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct ReadHistoryCensus {
    live_view_leases: usize,
    initialized_canonical_root_views: usize,
    canonical_root_items: usize,
    canonical_root_capacity_items: usize,
    local_root_items: usize,
    local_root_capacity_items: usize,
    lease_parent_links: usize,
    lease_parent_capacity_items: usize,
    materialization_parent_links: usize,
    materialization_parent_capacity_items: usize,
    initialized_read_projection_views: usize,
    projection_node_items: usize,
    projection_entry_items: usize,
    projection_entry_capacity_items: usize,
    initialized_dependency_vectors: usize,
    dependency_rows: usize,
    dependency_capacity_rows: usize,
    graph_nodes: usize,
    payload_entries: usize,
    root_registry_entries: usize,
    root_registry_references: usize,
    index_map_key_payload_bytes: usize,
    root_registry_key_payload_bytes: usize,
    projection_node_key_payload_bytes: usize,
    btree_key_payload_bytes_lower_bound: usize,
    local_root_vec_reserved_bytes: usize,
    parent_link_vec_reserved_bytes: usize,
    materialization_parent_vec_reserved_bytes: usize,
    canonical_root_vec_reserved_bytes: usize,
    read_entry_vec_reserved_bytes: usize,
    dependency_vec_reserved_bytes: usize,
    exact_vec_reserved_bytes: usize,
    size_inventory_node_key: usize,
    size_arc_artifact_entry: usize,
    size_dependency_row: usize,
    size_artifact_view: usize,
    size_materialization_arc: usize,
}

fn read_history_census(latest: &ArtifactView) -> ReadHistoryCensus {
    let mut pending = vec![latest];
    let mut views = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(view) = pending.pop() {
        if seen.insert(Arc::as_ptr(&view.0) as usize) {
            views.push(view);
            pending.extend(view.0.parents.iter());
        }
    }

    let inventory = latest.0.inventory.0.lock().expect("inventory lock");
    let mut census = ReadHistoryCensus {
        live_view_leases: views.len(),
        initialized_canonical_root_views: 0,
        canonical_root_items: 0,
        canonical_root_capacity_items: 0,
        local_root_items: 0,
        local_root_capacity_items: 0,
        lease_parent_links: 0,
        lease_parent_capacity_items: 0,
        materialization_parent_links: 0,
        materialization_parent_capacity_items: 0,
        initialized_read_projection_views: 0,
        projection_node_items: 0,
        projection_entry_items: 0,
        projection_entry_capacity_items: 0,
        initialized_dependency_vectors: 0,
        dependency_rows: 0,
        dependency_capacity_rows: 0,
        graph_nodes: inventory.graph.node_count(),
        payload_entries: inventory.payloads.len(),
        root_registry_entries: inventory.roots.len(),
        root_registry_references: inventory.roots.values().sum(),
        index_map_key_payload_bytes: inventory.indices.len()
            * std::mem::size_of::<InventoryNodeKey>(),
        root_registry_key_payload_bytes: inventory.roots.len()
            * std::mem::size_of::<InventoryNodeKey>(),
        projection_node_key_payload_bytes: 0,
        btree_key_payload_bytes_lower_bound: 0,
        local_root_vec_reserved_bytes: 0,
        parent_link_vec_reserved_bytes: 0,
        materialization_parent_vec_reserved_bytes: 0,
        canonical_root_vec_reserved_bytes: 0,
        read_entry_vec_reserved_bytes: 0,
        dependency_vec_reserved_bytes: 0,
        exact_vec_reserved_bytes: 0,
        size_inventory_node_key: std::mem::size_of::<InventoryNodeKey>(),
        size_arc_artifact_entry: std::mem::size_of::<Arc<ArtifactEntry>>(),
        size_dependency_row: std::mem::size_of::<(ArtifactId, ArtifactId, ArtifactDependency)>(),
        size_artifact_view: std::mem::size_of::<ArtifactView>(),
        size_materialization_arc: std::mem::size_of::<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >(),
    };
    for view in views {
        census.local_root_items += view.0.roots.len();
        census.local_root_capacity_items += view.0.roots.capacity();
        census.local_root_vec_reserved_bytes +=
            view.0.roots.capacity() * std::mem::size_of::<InventoryNodeKey>();
        census.lease_parent_links += view.0.parents.len();
        census.lease_parent_capacity_items += view.0.parents.capacity();
        census.parent_link_vec_reserved_bytes +=
            view.0.parents.capacity() * std::mem::size_of::<ArtifactView>();
        census.materialization_parent_links += view.0.materialization_parents.len();
        census.materialization_parent_capacity_items += view.0.materialization_parents.capacity();
        census.materialization_parent_vec_reserved_bytes +=
            view.0.materialization_parents.capacity()
                * std::mem::size_of::<
                    Arc<crate::declaration_context::RetainedArtifactMaterialization>,
                >();
        if let Some(roots) = view.0.canonical_roots.get() {
            census.initialized_canonical_root_views += 1;
            census.canonical_root_items += roots.len();
            census.canonical_root_capacity_items += roots.capacity();
            census.canonical_root_vec_reserved_bytes +=
                roots.capacity() * std::mem::size_of::<InventoryNodeKey>();
        }
        if let Some(projection) = view.0.read_projection.get() {
            census.initialized_read_projection_views += 1;
            census.projection_node_items += projection.nodes.len();
            census.projection_node_key_payload_bytes +=
                projection.nodes.len() * std::mem::size_of::<InventoryNodeKey>();
            census.projection_entry_items += projection.entries.len();
            census.projection_entry_capacity_items += projection.entries.capacity();
            census.read_entry_vec_reserved_bytes +=
                projection.entries.capacity() * std::mem::size_of::<Arc<ArtifactEntry>>();
            if let Some(dependencies) = projection.dependencies.get() {
                census.initialized_dependency_vectors += 1;
                census.dependency_rows += dependencies.len();
                census.dependency_capacity_rows += dependencies.capacity();
                census.dependency_vec_reserved_bytes += dependencies.capacity()
                    * std::mem::size_of::<(ArtifactId, ArtifactId, ArtifactDependency)>();
            }
        }
    }
    census.btree_key_payload_bytes_lower_bound = census.index_map_key_payload_bytes
        + census.root_registry_key_payload_bytes
        + census.projection_node_key_payload_bytes;
    census.exact_vec_reserved_bytes = census.local_root_vec_reserved_bytes
        + census.parent_link_vec_reserved_bytes
        + census.materialization_parent_vec_reserved_bytes
        + census.canonical_root_vec_reserved_bytes
        + census.read_entry_vec_reserved_bytes
        + census.dependency_vec_reserved_bytes;
    census
}

#[test]
fn linear_view_history_reports_retained_read_projection_cost() {
    const DEFAULT_LENGTH: usize = 128;
    const MAX_LENGTH: usize = 2048;
    let length = match std::env::var("TIDEPOOL_VIEW_READ_HISTORY_LENGTH") {
        Ok(value) => {
            let parsed = value.parse::<usize>().unwrap_or_else(|error| {
                panic!("invalid TIDEPOOL_VIEW_READ_HISTORY_LENGTH {value:?}: {error}")
            });
            assert!(
                (1..=MAX_LENGTH).contains(&parsed),
                "TIDEPOOL_VIEW_READ_HISTORY_LENGTH must be between 1 and {MAX_LENGTH}"
            );
            parsed
        }
        Err(std::env::VarError::NotPresent) => DEFAULT_LENGTH,
        Err(error) => panic!("cannot read TIDEPOOL_VIEW_READ_HISTORY_LENGTH: {error}"),
    };

    let inventory = ArtifactInventory::default();
    let mut latest = inventory.empty_view();
    let mut previous: Option<ArtifactDescriptor> = None;
    for index in 0..length {
        let unit = "retained-history".to_owned();
        let module = format!("History{index}");
        let requirements = previous
            .take()
            .map(|descriptor| {
                BTreeMap::from([(
                    (descriptor.owner.unit, descriptor.owner.module),
                    descriptor.interface_sha256,
                )])
            })
            .unwrap_or_default();
        let entry = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [0x57; 32],
            &unit,
            &module,
            requirements,
        ));
        previous = Some(entry.descriptor.clone());
        latest = inventory
            .admit(&latest, vec![entry])
            .expect("linear interface dependency admits");
        drop(latest.metadata_snapshot());
        drop(latest.entries());
        drop(latest.interface_owners());
    }

    let expected_triangular = length * (length + 1) / 2;
    let expected_dependency_rows = length * (length - 1) / 2;
    let before = read_history_census(&latest);
    assert_eq!(before.live_view_leases, length + 1);
    assert_eq!(before.initialized_canonical_root_views, length + 1);
    assert_eq!(before.canonical_root_items, expected_triangular);
    assert_eq!(before.local_root_items, length);
    assert_eq!(before.lease_parent_links, length);
    assert_eq!(before.materialization_parent_links, 0);
    assert_eq!(before.initialized_read_projection_views, length + 1);
    assert_eq!(before.projection_node_items, expected_triangular);
    assert_eq!(before.projection_entry_items, expected_triangular);
    assert_eq!(before.initialized_dependency_vectors, length);
    assert_eq!(before.dependency_rows, expected_dependency_rows);
    assert_eq!(before.graph_nodes, length);
    assert_eq!(before.payload_entries, length);
    assert_eq!(before.root_registry_entries, length);
    assert_eq!(before.root_registry_references, length);

    drop(latest.metadata_snapshot());
    drop(latest.entries());
    drop(latest.interface_owners());
    let after_repeated_warm_read = read_history_census(&latest);
    assert_eq!(after_repeated_warm_read, before);
    eprintln!(
        "{}",
        serde_json::json!({
            "kind": "artifact_view_read_history_census",
            "pid": std::process::id(),
            "history_length": length,
            "census": before,
            "after_repeated_warm_read": after_repeated_warm_read,
            "retained_btree_key_payload_bytes_excludes_node_overhead": true,
            "exact_vec_reserved_bytes_covers_retained_view_vectors_only": true,
            "dependency_owned_strings_included": false,
            "entry_payload_allocations_included": false,
            "lease_struct_and_once_lock_overhead_included": false,
        })
    );

    drop(latest);
    assert_eq!(inventory.node_count(), 0);
    let state = inventory.0.lock().expect("inventory lock");
    assert!(state.payloads.is_empty());
    assert!(state.roots.is_empty());
}
