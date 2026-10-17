//! Read projections are checked against a full graph fixed point and then
//! exercised repeatedly to ensure a warmed immutable view does not traverse
//! the inventory graph again.
use super::*;
use proptest::prelude::*;
use proptest::test_runner::{
    contextualize_config, FileFailurePersistence, TestCaseError, TestRunner,
};
use std::cell::RefCell;
use std::sync::{mpsc, Barrier};
use std::time::Duration;

const OWNERS: usize = 5;

struct Catalog {
    entries: Vec<ArtifactEntry>,
    dependency_facts: [u8; OWNERS],
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
        Self {
            entries,
            dependency_facts: *masks,
        }
    }

    fn owner(&self, index: usize) -> ExactModuleIdentity {
        self.entries[index].descriptor.owner.clone()
    }
}

fn check_empty_observation(view: &ArtifactView, empty: bool) -> Result<(), TestCaseError> {
    let roots_were_materialized = view.reads.canonical_roots.get().is_some();
    for _ in 0..12 {
        prop_assert_eq!(view.is_empty(), empty);
    }
    prop_assert_eq!(
        view.reads.canonical_roots.get().is_some(),
        roots_were_materialized,
        "emptiness must not materialize the inherited root union"
    );
    Ok(())
}

// This oracle starts from issued fixture operands, not the graph whose edge
// construction and cached getters are being checked. Logical indices are kept
// independent of production graph slots and content-id ordering.
fn check_constructed_projection(
    view: &ArtifactView,
    catalog: &Catalog,
    roots: &BTreeSet<usize>,
) -> Result<(), TestCaseError> {
    check_empty_observation(view, roots.is_empty())?;
    let mut reachable = roots.clone();
    loop {
        let prior = reachable.len();
        let next: Vec<_> = reachable
            .iter()
            .flat_map(|source| {
                (0..OWNERS).filter(move |target| {
                    *source != *target && catalog.dependency_facts[*source] & (1 << target) != 0
                })
            })
            .collect();
        reachable.extend(next);
        if reachable.len() == prior {
            break;
        }
    }
    let ids: BTreeSet<_> = reachable
        .iter()
        .map(|i| catalog.entries[*i].descriptor.id)
        .collect();
    let expected_roots: BTreeSet<_> = roots
        .iter()
        .map(|i| catalog.entries[*i].descriptor.id)
        .collect();
    let expected_edges: BTreeSet<_> = reachable
        .iter()
        .flat_map(|source| {
            reachable.iter().filter_map(move |target| {
                (*source != *target && catalog.dependency_facts[*source] & (1 << target) != 0)
                    .then_some((
                        catalog.entries[*source].descriptor.id,
                        catalog.entries[*target].descriptor.id,
                        ArtifactDependency::Interface,
                    ))
            })
        })
        .collect();
    prop_assert_eq!(
        view.artifact_ids().into_iter().collect::<BTreeSet<_>>(),
        ids.clone()
    );
    prop_assert_eq!(
        view.root_entries()
            .into_iter()
            .map(|entry| entry.descriptor.id)
            .collect::<BTreeSet<_>>(),
        expected_roots
    );
    prop_assert_eq!(
        view.dependencies().into_iter().collect::<BTreeSet<_>>(),
        expected_edges.clone()
    );
    let metadata = view.metadata_snapshot();
    let warm = view.lease.inventory.metrics();
    let repeated = view.clone().metadata_snapshot();
    prop_assert!(Arc::ptr_eq(&metadata, &repeated));
    prop_assert_eq!(
        view.lease.inventory.metrics().entry_handle_copies,
        warm.entry_handle_copies
    );
    prop_assert_eq!(
        metadata.artifacts.keys().copied().collect::<BTreeSet<_>>(),
        ids
    );
    prop_assert_eq!(
        metadata
            .dependencies()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        expected_edges
    );
    let selected: BTreeMap<_, _> = metadata
        .entries
        .iter()
        .map(|(owner, entry)| (owner.clone(), entry.descriptor.id))
        .collect();
    let expected: BTreeMap<_, _> = reachable
        .iter()
        .map(|i| {
            let descriptor = &catalog.entries[*i].descriptor;
            (descriptor.owner.clone(), descriptor.id)
        })
        .collect();
    prop_assert_eq!(selected, expected);
    Ok(())
}

#[derive(Default, Debug)]
struct ConstructionCoverage {
    callbacks: usize,
    completed_histories: usize,
    eager_reads: usize,
    delayed_first_reads: usize,
    sparse_reads: usize,
    independent_later_bindings: usize,
}

fn construction_history(
    catalog: &Catalog,
    root_mask: u8,
    read_schedule: &[bool; 5],
    coverage: &mut ConstructionCoverage,
) -> Result<(), TestCaseError> {
    let inventory = ArtifactInventory::default();
    let all = inventory
        .admit(&inventory.empty_view(), catalog.entries[..OWNERS].to_vec())
        .unwrap();
    let all_roots: BTreeSet<_> = (0..OWNERS).collect();
    check_empty_observation(&all, false)?;
    if read_schedule[0] {
        check_constructed_projection(&all, catalog, &all_roots)?;
    }
    let roots: BTreeSet<_> = (0..OWNERS - 1)
        .filter(|i| root_mask & (1 << i) != 0)
        .chain([0])
        .collect();
    let peer = all
        .select_roots(
            roots
                .iter()
                .map(|i| catalog.entries[*i].descriptor.id)
                .collect(),
        )
        .unwrap();
    check_empty_observation(&peer, false)?;
    if read_schedule[1] {
        check_constructed_projection(&peer, catalog, &roots)?;
    }
    let alias = peer.clone();
    if read_schedule[2] {
        check_constructed_projection(&alias, catalog, &roots)?;
    }
    let branch_roots = BTreeSet::from([OWNERS - 2]);
    let branch = all
        .select_roots(vec![catalog.entries[OWNERS - 2].descriptor.id])
        .unwrap();
    check_empty_observation(&branch, false)?;
    let merged = peer.merge_fixture_selections(&branch).unwrap();
    check_empty_observation(&merged, false)?;
    let merged_roots = roots.union(&branch_roots).copied().collect::<BTreeSet<_>>();
    if read_schedule[3] {
        check_constructed_projection(&merged, catalog, &merged_roots)?;
    }
    // The merged output has not served any production consumer. It can stay
    // cold while its parents retire and an unrelated binding is issued.
    let cold = merged.reads.read_projection.get().is_none();
    if read_schedule == &[false; 5] {
        prop_assert!(
            cold,
            "delayed output must remain cold until its first checked read"
        );
    }
    let retired = {
        let state = inventory.0.lock().unwrap();
        let binding = all
            .resolve_binding(
                catalog.entries[OWNERS - 1].descriptor.id,
                &all.read_projection(&state).nodes,
            )
            .unwrap();
        InventoryNodeKey::Artifact(binding)
    };
    drop(all);
    drop(branch);
    drop(alias);
    let unrelated = inventory
        .admit(&peer, vec![catalog.entries[OWNERS + 1].clone()])
        .unwrap();
    let reused = {
        let state = inventory.0.lock().unwrap();
        let binding = unrelated
            .resolve_binding(
                catalog.entries[OWNERS + 1].descriptor.id,
                &unrelated.read_projection(&state).nodes,
            )
            .unwrap();
        InventoryNodeKey::Artifact(binding)
    };
    prop_assert_ne!(
        reused,
        retired,
        "a later unrelated binding has independent identity"
    );
    coverage.independent_later_bindings += 1;
    if read_schedule[4] {
        check_constructed_projection(&peer, catalog, &roots)?;
    }
    check_constructed_projection(&merged, catalog, &merged_roots)?;
    check_constructed_projection(&peer, catalog, &roots)?;
    coverage.delayed_first_reads += usize::from(cold);
    drop(unrelated);
    drop(peer);
    drop(merged);
    prop_assert_eq!(inventory.node_count(), 0);
    Ok(())
}

#[test]
fn constructed_projection_matches_issued_facts_with_delayed_and_eager_reads() {
    let mut config = contextualize_config(property_config());
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::constructed_projection_matches_issued_facts_with_delayed_and_eager_reads"
    ));
    let configured_cases = config.cases;
    let coverage = RefCell::new(ConstructionCoverage::default());
    let initial_failure = RefCell::new(None);
    let strategy = (
        any::<[u8; OWNERS]>(),
        1u8..=u8::MAX,
        0u8..16,
        any::<[bool; 5]>(),
    );
    let result =
        TestRunner::new(config).run(&strategy, |(mut masks, seed, root_mask, schedule)| {
            coverage.borrow_mut().callbacks += 1;
            let input = (masks, seed, root_mask, schedule);
            let outcome = (|| {
                for mask in &mut masks[..OWNERS - 1] {
                    *mask &= 15;
                }
                masks[OWNERS - 1] = 0;
                masks[0] |= 1 << 1;
                masks[1] |= 1 << 2;
                let catalog = Catalog::new(&masks, seed);
                prop_assert_eq!(
                    catalog
                        .entries
                        .iter()
                        .map(|entry| entry.descriptor.id)
                        .collect::<BTreeSet<_>>()
                        .len(),
                    catalog.entries.len()
                );
                construction_history(&catalog, root_mask, &[true; 5], &mut coverage.borrow_mut())?;
                coverage.borrow_mut().eager_reads += 1;
                construction_history(&catalog, root_mask, &[false; 5], &mut coverage.borrow_mut())?;
                construction_history(&catalog, root_mask, &schedule, &mut coverage.borrow_mut())?;
                coverage.borrow_mut().sparse_reads += schedule.iter().filter(|read| **read).count();
                coverage.borrow_mut().completed_histories += 1;
                Ok(())
            })();
            if outcome.is_err() && initial_failure.borrow().is_none() {
                *initial_failure.borrow_mut() = Some(input);
            }
            outcome
        });
    eprintln!(
        "constructed_read_campaign configured_cases={configured_cases} observed={:?}",
        coverage.borrow()
    );
    eprintln!(
        "constructed_read_initial_failure={:?} minimized={:?}",
        initial_failure.borrow(),
        result
    );
    result.unwrap();
    let observed = coverage.into_inner();
    if observed.completed_histories > 0 {
        assert!(
            observed.delayed_first_reads >= observed.completed_histories,
            "{observed:?}"
        );
    }
}

#[derive(Default)]
struct Oracle {
    roots: BTreeSet<InventoryNodeKey>,
    closure: BTreeSet<InventoryNodeKey>,
    selected: BTreeSet<InventoryNodeKey>,
    entries: Vec<Arc<ArtifactEntry>>,
    dependencies: Vec<(ArtifactId, ArtifactId, ArtifactDependency)>,
}

/// This intentionally scans every graph node and repeatedly expands edge
/// facts. It does not call the production closure, index, or cached projection.
fn exhaustive_oracle(view: &ArtifactView) -> Oracle {
    let mut result = Oracle::default();
    result
        .roots
        .extend(view.lease.selection.roots.keys().copied());
    let custody = view
        .lease
        .selection
        .custody_roots
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut adjacency = BTreeMap::<InventoryNodeKey, BTreeSet<InventoryNodeKey>>::new();
    let mut edge_facts = Vec::new();
    for scope in view.lease.selection.scopes.values() {
        let proof = scope.proof();
        for node in &proof.nodes {
            adjacency.entry(*node).or_default();
        }
        for (source, target, label) in &proof.dependencies {
            adjacency.entry(*source).or_default().insert(*target);
            edge_facts.push((*source, *target, label.clone()));
        }
    }
    result.closure = result
        .roots
        .iter()
        .chain(custody.iter())
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
    result.selected = result.roots.clone();
    loop {
        let before = result.selected.len();
        let targets = result
            .selected
            .iter()
            .flat_map(|node| adjacency.get(node).into_iter().flatten().copied())
            .collect::<Vec<_>>();
        result.selected.extend(targets);
        if result.selected.len() == before {
            break;
        }
    }
    let artifact_ids = result
        .closure
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Artifact(binding) => Some(binding.artifact),
            InventoryNodeKey::Group(_) => None,
        })
        .collect::<BTreeSet<_>>();
    result.entries = artifact_ids
        .iter()
        .map(|id| Arc::clone(&view.lease.payloads[id]))
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
            if matches!((source, target), (InventoryNodeKey::Group(group), InventoryNodeKey::Artifact(carrier)) if carrier.artifact == group.binding.artifact())
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
        .selected
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Group(group) => Some(NativeGroupKey {
                artifact: group.binding.artifact(),
                original_ordinal: group.original_ordinal,
            }),
            InventoryNodeKey::Artifact(_) => None,
        })
        .collect::<BTreeSet<_>>();
    let expected_roots = expected
        .roots
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Artifact(binding) => Some(binding.artifact),
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
    let warm = view.lease.inventory.metrics();
    let repeated = view.clone().metadata_snapshot();
    prop_assert!(Arc::ptr_eq(&metadata, &repeated));
    prop_assert_eq!(
        view.lease.inventory.metrics().entry_handle_copies,
        warm.entry_handle_copies
    );
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
        // Zero is reserved by the certificate decoder and cannot represent a
        // well-formed fixture producer identity.
        seed in 1u8..=u8::MAX,
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
            let projected = {
                let entries = catalog.entries[..OWNERS].iter().cloned().map(Arc::new).collect::<Vec<_>>();
                let projection = CompilerInputProjection::from_issued_entries(&entries)
                    .unwrap()
                    .within_view(&selected);
                let issued_namespace = selected.with_compiler_projection(&projection).unwrap();
                let support = crate::declaration_context::recover_fixture_compiler_support(
                    projection,
                    issued_namespace,
                    [],
                ).unwrap();
                let issued = support.selected_interfaces().unwrap()
                    .select_owners(vec![catalog.owner(*index)]).unwrap();
                selected.select_issued(
                    issued.root_entries().iter().map(|entry| entry.descriptor.id).collect(),
                    &ExactArtifactSelection::capture(&issued),
                ).unwrap()
            };
            check_read_projection(&projected)?;
            prop_assert!(
                projected.selected_native_groups().is_empty(),
                "interface projection carries no native groups"
            );
            selected = selected.merge_fixture_selections(&projected).unwrap();
            check_read_projection(&selected)?;
        }

        // Exercise parent-linked same-inventory merging and cross-inventory
        // admission of identical content IDs from an independent graph.
        let same_merged = peer.merge_fixture_selections(&extended).unwrap();
        check_read_projection(&same_merged)?;
        let other = right.admit(&right.empty_view(), other_catalog.entries[..OWNERS].to_vec()).unwrap();
        let cross_merged = all.merge_fixture_selections(&other).unwrap();
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
        let interface = {
            let entries = catalog.entries[..=OWNERS].iter().cloned().map(Arc::new).collect::<Vec<_>>();
            let projection = CompilerInputProjection::from_issued_entries(&entries)
                .unwrap()
                .interface_only();
            let issued_namespace = extended.with_compiler_projection(&projection).unwrap();
            let support = crate::declaration_context::recover_fixture_compiler_support(
                projection,
                issued_namespace,
                [],
            ).unwrap();
            let types = support.selected_interfaces().unwrap()
                .support_for_owners(vec![interface_owner.clone()]).unwrap();
            let issued = types.artifacts();
            extended.select_issued(
                issued.root_entries().iter().map(|entry| entry.descriptor.id).collect(),
                &ExactArtifactSelection::capture(issued),
            ).unwrap()
        };
        prop_assert!(
            interface.selected_native_groups().is_empty(),
            "projected view has no selected native groups"
        );
        prop_assert!(
            interface.metadata_snapshot().selected_native_groups.is_empty(),
            "projected metadata has no selected native groups"
        );
        prop_assert!(
            interface.lease.materialization.fixed.is_empty(),
            "projection has no materialization parents"
        );
        prop_assert!(
            interface.lease.materialization.cache.lock().unwrap().is_empty(),
            "projection has no retained materializations"
        );
        let projected_entries = interface.entries();
        prop_assert!(
            interface.root_entries().iter().all(|entry| !entry.is_native()),
            "projection selects only interface roots"
        );
        prop_assert!(
            interface.namespace().keys().all(|id| !interface.lease.payloads[id].is_native()),
            "projection selects only interface compiler roles"
        );
        prop_assert!(
            projected_entries.iter().any(|entry|
                entry.descriptor.id == catalog.entries[OWNERS].descriptor.id && entry.is_native()),
            "projection retains the exact original carrier as private custody"
        );
        let retains_interface = projected_entries
            .iter()
            .any(|entry| entry.descriptor.owner == interface_owner);
        prop_assert!(retains_interface, "projection retains the selected interface");
        check_read_projection(&interface)?;
        drop(interface);
        let initial_indices = left
            .0
            .lock()
            .unwrap()
            .live_nodes();
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
            .live_nodes();
        let removed_indices = initial_indices
            .difference(&remaining_indices)
            .copied()
            .collect::<BTreeSet<_>>();
        prop_assert!(!removed_indices.is_empty(), "history must reclaim graph nodes");
        let reused = left
            .admit(&peer, vec![catalog.entries[OWNERS + 1].clone()])
            .unwrap();
        let reused_index = {
            let state = left.0.lock().unwrap();
            let binding = reused.resolve_binding(
                catalog.entries[OWNERS + 1].descriptor.id,
                &reused.read_projection(&state).nodes,
            ).unwrap();
            InventoryNodeKey::Artifact(binding)
        };
        prop_assert!(!remaining_indices.contains(&reused_index), "later admission must not alias a surviving binding");
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

#[test]
fn empty_observation_preserves_inherited_roots_and_separate_native_custody() {
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let all = inventory
        .admit(&empty, Catalog::new(&[0; OWNERS], 1).entries)
        .unwrap();
    let inherited = inventory.retain(Vec::new(), Vec::new(), vec![all.lease.clone()], Vec::new());
    assert!(!inherited.selection_snapshot().roots.is_empty());
    check_empty_observation(&inherited, false).unwrap();
    check_empty_observation(&empty, true).unwrap();
    for merged in [
        empty.merge_fixture_selections(&inherited).unwrap(),
        inherited.merge_fixture_selections(&empty).unwrap(),
    ] {
        assert!(Arc::ptr_eq(&merged.lease, &inherited.lease));
        check_empty_observation(&merged, false).unwrap();
    }

    let native = all
        .lease
        .selection
        .roots
        .keys()
        .find_map(|root| match root {
            InventoryNodeKey::Group(group) => Some(group.binding.artifact()),
            _ => None,
        })
        .expect("fixture admits native groups");
    let carrier = {
        let state = inventory.0.lock().unwrap();
        all.resolve_binding(native, &all.read_projection(&state).nodes)
            .unwrap()
    };
    let mut custody = inventory.retain(
        Vec::new(),
        vec![InventoryNodeKey::Artifact(carrier)],
        Vec::new(),
        Vec::new(),
    );
    custody.replace_namespace(&BTreeMap::from([(carrier.artifact, carrier)]));
    assert!(!custody.namespace().is_empty());
    let inherited_custody = inventory.retain(
        Vec::new(),
        Vec::new(),
        vec![custody.lease.clone()],
        Vec::new(),
    );
    check_empty_observation(&custody, true).unwrap();
    check_empty_observation(&inherited_custody, true).unwrap();
    assert!(!inherited_custody.lease.selection.custody_roots.is_empty());
    assert!(!inherited_custody.entries().is_empty());
    check_empty_observation(&inherited_custody, true).unwrap();
    drop(inherited_custody);
    drop(custody);
    drop(inherited);
    drop(all);
    drop(empty);
    assert_eq!(inventory.node_count(), 0);
}

#[test]
fn zero_mask_empty_selection_boundary_releases_valid_fixture_views() {
    let catalog = Catalog::new(&[0; OWNERS], 1);
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let all = inventory
        .admit(&empty, catalog.entries[..OWNERS].to_vec())
        .unwrap();
    let extended = inventory
        .admit(&all, vec![catalog.entries[OWNERS].clone()])
        .unwrap();

    check_read_projection(&all).unwrap();
    check_read_projection(&extended).unwrap();
    let snapshot = extended.metadata_snapshot();
    assert!(!snapshot.selected_native_groups.is_empty());
    let selected_empty = all.select_roots(Vec::new()).unwrap();
    assert!(selected_empty.is_empty());
    check_read_projection(&selected_empty).unwrap();

    let native_interface = {
        let entries = catalog.entries[..=OWNERS]
            .iter()
            .cloned()
            .map(Arc::new)
            .collect::<Vec<_>>();
        let projection = CompilerInputProjection::from_issued_entries(&entries)
            .unwrap()
            .interface_only();
        let issued_namespace = extended.with_compiler_projection(&projection).unwrap();
        let support = crate::declaration_context::recover_fixture_compiler_support(
            projection,
            issued_namespace,
            [],
        )
        .unwrap();
        let types = support
            .selected_interfaces()
            .unwrap()
            .support_for_owners(vec![catalog.owner(OWNERS)])
            .unwrap();
        let issued = types.artifacts();
        extended
            .select_issued(
                issued
                    .root_entries()
                    .iter()
                    .map(|entry| entry.descriptor.id)
                    .collect(),
                &ExactArtifactSelection::capture(issued),
            )
            .unwrap()
    };
    assert!(native_interface.selected_native_groups().is_empty());
    assert!(native_interface
        .root_entries()
        .iter()
        .all(|entry| !entry.is_native()));
    assert!(native_interface
        .namespace()
        .keys()
        .all(|id| !native_interface.lease.payloads[id].is_native()));
    assert!(native_interface
        .entries()
        .iter()
        .any(
            |entry| entry.descriptor.id == catalog.entries[OWNERS].descriptor.id
                && entry.is_native()
        ));
    check_read_projection(&native_interface).unwrap();

    drop(all);
    drop(extended);
    drop(selected_empty);
    drop(native_interface);
    drop(empty);
    assert_eq!(inventory.node_count(), 0);
    let state = inventory.0.lock().expect("inventory lock");
    assert!(state.payload_count() == 0);
    assert!(state.live_nodes().is_empty());
}

#[test]
fn cloned_view_shares_only_its_explicit_read_cache_lifetime() {
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let view = inventory
        .admit(&empty, Catalog::new(&[0; OWNERS], 1).entries)
        .expect("valid interface and native fixtures admit");
    check_read_projection(&view).unwrap();

    let clone = view.clone();
    assert!(Arc::ptr_eq(&view.lease, &clone.lease));
    assert!(Arc::ptr_eq(&view.reads, &clone.reads));
    let reads = Arc::downgrade(&view.reads);
    let lease = Arc::downgrade(&view.lease);
    drop(view);
    assert!(reads.upgrade().is_some());
    drop(clone);
    assert!(reads.upgrade().is_none());
    assert!(lease.upgrade().is_none());

    drop(empty);
    assert_eq!(inventory.node_count(), 0);
}

#[test]
fn same_inventory_merge_keeps_custody_after_peer_read_caches_drop() {
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let all = inventory
        .admit(&empty, Catalog::new(&[0; OWNERS], 1).entries)
        .expect("valid interface and native fixtures admit");
    let entries = all.entries();
    let left = all
        .select_roots(vec![entries[0].descriptor.id])
        .expect("left selected root belongs to the view");
    let right = all
        .select_roots(vec![entries[1].descriptor.id])
        .expect("right selected root belongs to the view");
    check_read_projection(&left).unwrap();
    check_read_projection(&right).unwrap();

    let left_reads = Arc::downgrade(&left.reads);
    let right_reads = Arc::downgrade(&right.reads);
    let left_lease = Arc::downgrade(&left.lease);
    let right_lease = Arc::downgrade(&right.lease);
    let merged = left
        .merge_fixture_selections(&right)
        .expect("same-inventory merge succeeds");
    check_read_projection(&merged).unwrap();
    assert!(!Arc::ptr_eq(&merged.reads, &left.reads));
    assert!(!Arc::ptr_eq(&merged.reads, &right.reads));

    drop(left);
    drop(right);
    assert!(left_reads.upgrade().is_none());
    assert!(right_reads.upgrade().is_none());
    assert!(left_lease.upgrade().is_some());
    assert!(right_lease.upgrade().is_some());

    drop(all);
    drop(empty);
    assert_eq!(
        inventory.node_count(),
        exhaustive_oracle(&merged).closure.len()
    );
    check_read_projection(&merged).unwrap();
    drop(merged);
    assert_eq!(inventory.node_count(), 0);
    let state = inventory.0.lock().expect("inventory lock");
    assert!(state.payload_count() == 0);
    assert!(state.live_nodes().is_empty());
}

#[test]
fn persistent_payload_history_releases_ancestors_and_shares_immutable_entries() {
    const LENGTH: usize = 64;
    let inventory = ArtifactInventory::default();
    let mut latest = inventory.empty_view();
    let mut ancestors = Vec::new();
    let mut payload_owners = Vec::new();
    for index in 0..LENGTH {
        let entry = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [0x59; 32],
            "payload-history",
            &format!("Capture{index}"),
            BTreeMap::new(),
        ));
        let id = entry.descriptor.id;
        latest = inventory.admit(&latest, vec![entry]).unwrap();
        ancestors.push(Arc::downgrade(&latest.lease));
        payload_owners.push(Arc::downgrade(&latest.lease.payloads[&id]));
    }
    assert!(ancestors[..LENGTH - 1]
        .iter()
        .all(|owner| owner.upgrade().is_none()));
    assert_eq!(latest.lease.payloads.len(), LENGTH);
    assert_eq!(latest.lease.selection.scopes.len(), LENGTH);
    let retained_payload_bytes = latest
        .lease
        .payloads
        .values()
        .map(|entry| {
            let ArtifactPayload::Canonical(interface) = &entry.payload else {
                unreachable!()
            };
            interface.interface_bytes().len()
                + interface.package_imports_bytes().len()
                + interface.certificate_bytes().len()
                + interface.core_bytes().map_or(0, <[u8]>::len)
        })
        .sum::<usize>();
    assert!(retained_payload_bytes > 0);
    let narrowed = latest
        .select_roots(vec![*latest.lease.payloads.keys().next().unwrap()])
        .unwrap();
    assert_eq!(narrowed.lease.payloads.len(), 1);
    let proof = Arc::clone(narrowed.selection_snapshot());
    let before = inventory.metrics().graph_visits;
    drop(latest);
    assert_eq!(inventory.node_count(), 1);
    drop(narrowed);
    assert_eq!(inventory.node_count(), 0);
    assert_eq!(inventory.metrics().graph_visits, before);
    assert!(inventory.0.lock().unwrap().payload_count() == 0);
    assert!(payload_owners.iter().all(|owner| owner.upgrade().is_none()));
    assert!(!proof.scopes.is_empty());
    eprintln!(
        "{}",
        serde_json::json!({"history_length": LENGTH, "payload_entries": LENGTH,
        "narrowed_payload_entries": 1, "retained_parent_leases": 0, "retained_payload_bytes": retained_payload_bytes, "drop_graph_visits": 0})
    );
}

#[test]
fn persistent_captures_share_payload_and_scope_owners() {
    const LENGTH: usize = 32;
    let inventory = ArtifactInventory::default();
    let mut latest = inventory.empty_view();
    let mut captures = Vec::new();
    let mut payloads = Vec::new();
    for index in 0..LENGTH {
        let entry = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [0x63; 32],
            "captured-payloads",
            &format!("Capture{index}"),
            BTreeMap::new(),
        ));
        let id = entry.descriptor.id;
        latest = inventory.admit(&latest, vec![entry]).unwrap();
        payloads.push(Arc::downgrade(&latest.lease.payloads[&id]));
        captures.push(latest.clone());
    }
    let selected_references = captures
        .iter()
        .map(|capture| capture.lease.payloads.len())
        .sum::<usize>();
    assert_eq!(selected_references, LENGTH * (LENGTH + 1) / 2);
    for capture in &captures {
        for (id, entry) in &capture.lease.payloads {
            assert!(Arc::ptr_eq(entry, &latest.lease.payloads[id]));
        }
        for (scope, proof) in &capture.selection_snapshot().scopes {
            assert!(Arc::ptr_eq(
                proof.proof(),
                latest.selection_snapshot().scopes[scope].proof()
            ));
        }
    }
    let proofs = captures
        .iter()
        .map(|capture| Arc::clone(capture.selection_snapshot()))
        .collect::<Vec<_>>();
    let before = inventory.metrics().graph_visits;
    drop(latest);
    drop(captures);
    assert!(payloads.iter().all(|payload| payload.upgrade().is_none()));
    assert_eq!(inventory.node_count(), 0);
    assert_eq!(inventory.metrics().graph_visits, before);
    assert_eq!(proofs.len(), LENGTH);
    eprintln!(
        "{}",
        serde_json::json!({"explicit_captures": LENGTH, "selected_references": selected_references,
        "unique_payload_owners": LENGTH, "unique_scope_owners": LENGTH, "drop_graph_visits": 0})
    );
}

#[test]
fn ordered_materialization_sources_share_direct_edges_through_linear_and_diamond_histories() {
    for diamond in [false, true] {
        for length in [1, 8, 32] {
            let inventory = ArtifactInventory::default();
            let mut latest = inventory.empty_view();
            let mut captures = vec![latest.clone()];
            let mut leases = Vec::new();
            let mut caches = vec![Arc::downgrade(&latest.lease.materialization.cache)];
            for _ in 0..length {
                leases.push(Arc::downgrade(&latest.lease));
                if diamond {
                    let left =
                        inventory.retain(vec![], vec![], vec![Arc::clone(&latest.lease)], vec![]);
                    let right =
                        inventory.retain(vec![], vec![], vec![Arc::clone(&latest.lease)], vec![]);
                    caches.push(Arc::downgrade(&left.lease.materialization.cache));
                    caches.push(Arc::downgrade(&right.lease.materialization.cache));
                    latest = inventory.retain(
                        vec![],
                        vec![],
                        vec![Arc::clone(&left.lease), Arc::clone(&right.lease)],
                        vec![],
                    );
                } else {
                    latest =
                        inventory.retain(vec![], vec![], vec![Arc::clone(&latest.lease)], vec![]);
                }
                caches.push(Arc::downgrade(&latest.lease.materialization.cache));
                captures.push(latest.clone());
            }
            let nodes = latest.lease.materialization_sources.len() + 1;
            let direct_edges = latest.lease.materialization.parents.len()
                + latest
                    .lease
                    .materialization_sources
                    .values()
                    .map(|source| source.parents.len())
                    .sum::<usize>();
            assert_eq!(nodes, 1 + length * if diamond { 3 } else { 1 });
            assert_eq!(direct_edges, length * if diamond { 4 } else { 1 });
            let mut expanded_source_references = 0;
            for capture in &captures {
                expanded_source_references += capture.lease.materialization_sources.len() + 1;
                for (id, source) in &capture.lease.materialization_sources {
                    let shared = &latest.lease.materialization_sources[id];
                    assert!(Arc::ptr_eq(&source.cache, &shared.cache));
                    assert!(Arc::ptr_eq(&source.parents, &shared.parents));
                    assert!(source
                        .parents
                        .iter()
                        .all(|parent| capture.lease.materialization_sources.contains_key(parent)));
                }
                assert!(capture
                    .lease
                    .materialization
                    .parents
                    .iter()
                    .all(|parent| capture.lease.materialization_sources.contains_key(parent)));
            }
            drop(captures);
            assert!(leases.iter().all(|lease| lease.upgrade().is_none()));
            let proof = Arc::clone(latest.selection_snapshot());
            drop(latest);
            assert!(caches.iter().all(|cache| cache.upgrade().is_none()));
            eprintln!(
                "{}",
                serde_json::json!({
                    "kind": "ordered_materialization_source_structure", "shape": if diamond { "diamond" } else { "linear" },
                    "history_length": length, "retained_cache_nodes": nodes, "unique_direct_edges": direct_edges,
                    "direct_edge_id_bytes_excludes_arc_and_map_nodes": direct_edges * std::mem::size_of::<MaterializationCacheId>(),
                    "explicit_capture_source_references": expanded_source_references, "shared_direct_edge_arrays": true,
                    "retained_parent_leases": 0, "cache_owners_after_last_view": 0,
                    "proof_scope_count_after_drop": proof.scopes.len(),
                })
            );
        }
    }
}
