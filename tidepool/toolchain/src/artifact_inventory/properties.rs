//! Small histories compare the retained-view contract with sets of immutable
//! artifacts. The model recomputes live closure after each operation; it has no
//! graph indices, reference counts, incoming-edge reclamation or lease tree.
use super::*;
use proptest::prelude::*;
use proptest::test_runner::{FileFailurePersistence, TestCaseError};

const OWNERS: usize = 6;
const SLOTS: usize = 8;

fn owner(key: usize) -> ExactModuleIdentity {
    ExactModuleIdentity {
        unit: format!("unit{}", key / 3),
        module: format!("Module{}", key % 3),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Spec {
    key: usize,
    producer: u8,
    joined: bool,
    requirements: BTreeSet<usize>,
}

impl Spec {
    fn entry(&self) -> ArtifactEntry {
        let exact = owner(self.key);
        let interface = crate::certified_products::fixture_module_interface(
            [self.producer; 32],
            &exact.unit,
            &exact.module,
            self.requirements
                .iter()
                .map(|key| {
                    let exact = owner(*key);
                    let seal = digest(exact.module.as_bytes());
                    ((exact.unit, exact.module), seal)
                })
                .collect(),
        );
        if self.joined {
            ArtifactEntry::interface(
                CertifiedJoinedInterface::from_certification(
                    [self.producer; 32],
                    exact.unit,
                    exact.module,
                    interface.interface_bytes().to_vec(),
                    interface.package_imports_bytes().to_vec(),
                )
                .unwrap(),
                JoinedInterfaceRole::LexicalJoin,
                self.requirements.iter().map(|key| owner(*key)).collect(),
            )
        } else {
            ArtifactEntry::canonical(interface)
        }
    }
}

struct Catalog {
    specs: Vec<Spec>,
    entries: Vec<ArtifactEntry>,
}

impl Catalog {
    fn new(masks: &[u8]) -> Self {
        let mut specs = Vec::new();
        // Canonical finalization forbids an owner requiring itself; cycles
        // between distinct exact owners remain valid graph inputs.
        for producer in [2, 3] {
            specs.extend(masks.iter().enumerate().map(|(key, mask)| {
                Spec {
                    key,
                    producer,
                    joined: false,
                    requirements: (0..OWNERS)
                        .filter(|bit| *bit != key && mask & (1 << bit) != 0)
                        .collect(),
                }
            }));
        }
        // Joined interface bytes do not encode lexical requirements. Changing
        // their graph metadata must refuse reuse of the same content identity.
        specs.extend(
            [BTreeSet::from([0, 1]), BTreeSet::from([0])].map(|requirements| Spec {
                key: OWNERS,
                producer: 2,
                joined: true,
                requirements,
            }),
        );
        let entries = specs.iter().map(Spec::entry).collect();
        Self { specs, entries }
    }
}

#[derive(Clone, Debug)]
enum Op {
    Empty {
        inventory: usize,
        to: usize,
    },
    Admit {
        inventory: usize,
        parent: usize,
        entries: Vec<usize>,
        to: usize,
    },
    Clone {
        from: usize,
        to: usize,
    },
    Drop(usize),
    Select {
        from: usize,
        roots: Vec<usize>,
        outside: bool,
        to: usize,
    },
    Project {
        from: usize,
        owners: Vec<usize>,
        to: usize,
    },
    Merge {
        left: usize,
        right: usize,
        to: usize,
    },
}

fn operations() -> impl Strategy<Value = Vec<Op>> {
    let slot = 0..SLOTS;
    let indices = proptest::collection::vec(0..(2 * OWNERS + 2), 0..5);
    let op = prop_oneof![
        1 => (0usize..2, slot.clone()).prop_map(|(inventory, to)| Op::Empty { inventory, to }),
        5 => (0usize..2, slot.clone(), indices.clone(), slot.clone())
            .prop_map(|(inventory, parent, entries, to)| Op::Admit { inventory, parent, entries, to }),
        2 => (slot.clone(), slot.clone()).prop_map(|(from, to)| Op::Clone { from, to }),
        3 => slot.clone().prop_map(Op::Drop),
        4 => (slot.clone(), indices, any::<bool>(), slot.clone())
            .prop_map(|(from, roots, outside, to)| Op::Select { from, roots, outside, to }),
        2 => (slot.clone(), proptest::collection::vec(0..(OWNERS + 2), 0..4), slot.clone())
            .prop_map(|(from, owners, to)| Op::Project { from, owners, to }),
        3 => (slot.clone(), slot.clone(), slot)
            .prop_map(|(left, right, to)| Op::Merge { left, right, to }),
    ];
    proptest::collection::vec(op, 0..49)
}

#[derive(Clone, Debug)]
struct View {
    inventory: usize,
    roots: BTreeSet<ArtifactId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Record {
    spec: Spec,
    descriptor: ArtifactDescriptor,
    dependencies: BTreeSet<ArtifactId>,
}

#[derive(Default)]
struct Model {
    records: [BTreeMap<ArtifactId, Record>; 2],
    views: [Option<View>; SLOTS],
}

// Repeated expansion of a finite set deliberately differs from the production
// stack walk. It also handles cycles and duplicate roots without special cases.
fn reachable(
    records: &BTreeMap<ArtifactId, Record>,
    roots: &BTreeSet<ArtifactId>,
) -> BTreeSet<ArtifactId> {
    let mut ids = roots.clone();
    loop {
        let before = ids.len();
        let targets: Vec<_> = ids
            .iter()
            .flat_map(|id| records[id].dependencies.iter().copied())
            .collect();
        ids.extend(targets);
        if ids.len() == before {
            return ids;
        }
    }
}

#[derive(Debug)]
enum Refusal {
    Foreign,
    Typed(Vec<ArtifactInventoryFailure>),
}

impl Model {
    fn contents(&self, view: &View) -> BTreeSet<ArtifactId> {
        reachable(&self.records[view.inventory], &view.roots)
    }

    fn admission(
        &mut self,
        inventory: usize,
        parent: &View,
        incoming: &[(Spec, ArtifactDescriptor)],
    ) -> Result<View, Refusal> {
        if inventory != parent.inventory {
            return Err(Refusal::Foreign);
        }
        if incoming.is_empty() {
            return Ok(parent.clone());
        }
        let stored = &self.records[inventory];
        let mut supplied = BTreeMap::<ArtifactId, (Spec, ArtifactDescriptor)>::new();
        let mut failures = Vec::new();
        for (spec, descriptor) in incoming {
            let id = descriptor.id;
            let differs = stored
                .get(&id)
                .is_some_and(|old| old.spec != *spec || old.descriptor != *descriptor)
                || supplied
                    .get(&id)
                    .is_some_and(|old| old != &(spec.clone(), descriptor.clone()));
            if differs {
                failures.push(ArtifactInventoryFailure::MetadataConflict { artifact: id });
            }
            supplied.insert(id, (spec.clone(), descriptor.clone()));
        }
        if !failures.is_empty() {
            return Err(Refusal::Typed(failures));
        }
        let roots: BTreeSet<_> = supplied.keys().copied().collect();
        let reused: BTreeSet<_> = roots
            .iter()
            .filter(|id| stored.contains_key(id))
            .copied()
            .collect();
        let mut selected = self.contents(parent);
        selected.extend(reachable(stored, &reused));
        let mut entries: BTreeMap<_, _> = selected
            .into_iter()
            .map(|id| {
                let record = &stored[&id];
                (id, (record.spec.clone(), record.descriptor.clone()))
            })
            .collect();
        entries.extend(supplied);
        let mut owners = BTreeMap::<usize, Vec<ArtifactId>>::new();
        for (id, (spec, _)) in &entries {
            owners.entry(spec.key).or_default().push(*id);
        }
        for (key, ids) in &owners {
            if ids.len() > 1 {
                failures.push(ArtifactInventoryFailure::OwnerConflict { owner: owner(*key) });
            }
        }
        if !failures.is_empty() {
            return Err(Refusal::Typed(failures));
        }
        let mut planned = BTreeMap::new();
        for (id, (spec, descriptor)) in &entries {
            let mut dependencies = BTreeSet::new();
            for required in &spec.requirements {
                match owners.get(required) {
                    None => failures.push(ArtifactInventoryFailure::MissingDependency {
                        artifact: *id,
                        dependent: owner(spec.key),
                        required: owner(*required),
                        dependency: ArtifactDependency::Interface,
                    }),
                    Some(ids) => {
                        let target = ids[0];
                        let (target_spec, _) = &entries[&target];
                        if !spec.joined && target_spec.producer != spec.producer {
                            failures.push(ArtifactInventoryFailure::InterfaceSealMismatch {
                                dependent: owner(spec.key),
                                required: owner(*required),
                            });
                        }
                        dependencies.insert(target);
                    }
                }
            }
            if stored
                .get(id)
                .is_some_and(|old| old.dependencies != dependencies)
            {
                failures.push(ArtifactInventoryFailure::MetadataConflict { artifact: *id });
            }
            planned.insert(
                *id,
                Record {
                    spec: spec.clone(),
                    descriptor: descriptor.clone(),
                    dependencies,
                },
            );
        }
        if !failures.is_empty() {
            return Err(Refusal::Typed(failures));
        }
        self.records[inventory].extend(planned);
        Ok(View {
            inventory,
            roots: parent.roots.union(&roots).copied().collect(),
        })
    }

    fn reclaim(&mut self) -> usize {
        let mut removed = 0;
        for inventory in 0..2 {
            let roots = self
                .views
                .iter()
                .flatten()
                .filter(|v| v.inventory == inventory)
                .flat_map(|v| v.roots.iter().copied())
                .collect();
            let live = reachable(&self.records[inventory], &roots);
            let before = self.records[inventory].len();
            self.records[inventory].retain(|id, _| live.contains(id));
            removed += before - live.len();
        }
        removed
    }
}

#[derive(Default, Debug)]
struct Coverage {
    admitted: usize,
    reused: usize,
    metadata_refused: usize,
    owner_refused: usize,
    dependency_refused: usize,
    seal_refused: usize,
    foreign_refused: usize,
    selected: usize,
    outside_refused: usize,
    cloned: usize,
    merged: usize,
    cross_merged: usize,
    reclaimed: usize,
}

fn check_refusal(
    expected: Refusal,
    actual: CompileError,
    coverage: &mut Coverage,
) -> Result<(), TestCaseError> {
    match expected {
        Refusal::Foreign => {
            prop_assert!(
                matches!(actual, CompileError::ExtractFailed(_)),
                "foreign admission: {actual:?}"
            );
            coverage.foreign_refused += 1;
        }
        Refusal::Typed(allowed) => {
            let CompileError::ArtifactInventory(error) = actual else {
                return Err(TestCaseError::fail(format!(
                    "expected typed refusal {allowed:?}, got {actual:?}"
                )));
            };
            prop_assert!(
                allowed.contains(&error.failure),
                "expected one of {allowed:?}, got {:?}",
                error.failure
            );
            match error.failure {
                ArtifactInventoryFailure::MetadataConflict { .. } => coverage.metadata_refused += 1,
                ArtifactInventoryFailure::OwnerConflict { .. } => coverage.owner_refused += 1,
                ArtifactInventoryFailure::MissingDependency { .. } => {
                    coverage.dependency_refused += 1
                }
                ArtifactInventoryFailure::InterfaceSealMismatch { .. } => {
                    coverage.seal_refused += 1
                }
                other => {
                    return Err(TestCaseError::fail(format!(
                        "unexpected refusal: {other:?}"
                    )))
                }
            }
        }
    }
    Ok(())
}

fn observe(
    model: &Model,
    inventories: &[ArtifactInventory; 2],
    views: &[Option<ArtifactView>; SLOTS],
) -> Result<(), TestCaseError> {
    for (inventory, actual) in inventories.iter().enumerate() {
        prop_assert_eq!(actual.node_count(), model.records[inventory].len());
        prop_assert_eq!(actual.metrics().nodes, model.records[inventory].len());
    }
    for (slot, expected) in model.views.iter().enumerate() {
        let Some(expected) = expected else {
            prop_assert!(views[slot].is_none());
            continue;
        };
        let actual = views[slot].as_ref().unwrap();
        let ids = model.contents(expected);
        let records = &model.records[expected.inventory];
        let descriptors: BTreeMap<_, _> = ids
            .iter()
            .map(|id| (*id, records[id].descriptor.clone()))
            .collect();
        let edges: BTreeSet<_> = ids
            .iter()
            .flat_map(|id| {
                records[id]
                    .dependencies
                    .iter()
                    .map(|target| (*id, *target, ArtifactDependency::Interface))
            })
            .collect();
        prop_assert_eq!(actual.artifact_ids().len(), ids.len());
        prop_assert_eq!(actual.descriptors().len(), descriptors.len());
        prop_assert_eq!(actual.dependencies().len(), edges.len());
        prop_assert_eq!(actual.interface_dependencies().len(), edges.len());
        prop_assert_eq!(
            actual.artifact_ids().into_iter().collect::<BTreeSet<_>>(),
            ids.clone()
        );
        prop_assert_eq!(
            actual
                .descriptors()
                .into_iter()
                .map(|d| (d.id, d))
                .collect::<BTreeMap<_, _>>(),
            descriptors.clone()
        );
        prop_assert_eq!(
            actual.dependencies().into_iter().collect::<BTreeSet<_>>(),
            edges.clone()
        );
        prop_assert_eq!(
            actual
                .interface_dependencies()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            edges.clone()
        );
        prop_assert_eq!(
            actual
                .root_entries()
                .into_iter()
                .map(|e| e.descriptor.id)
                .collect::<BTreeSet<_>>(),
            expected.roots.clone()
        );
        prop_assert_eq!(actual.is_empty(), expected.roots.is_empty());
        let snapshot = actual.metadata_snapshot();
        prop_assert_eq!(
            snapshot
                .descriptors()
                .into_iter()
                .map(|d| (d.id, d.clone()))
                .collect::<BTreeMap<_, _>>(),
            descriptors
        );
        prop_assert_eq!(
            snapshot.dependencies().into_iter().collect::<BTreeSet<_>>(),
            edges
        );
        prop_assert!(snapshot.ambiguous_native_owners.is_empty());
        prop_assert!(snapshot.validate_native_selection().is_ok());
        let owners: BTreeMap<_, _> = ids
            .iter()
            .map(|id| (records[id].descriptor.owner.clone(), *id))
            .collect();
        prop_assert_eq!(
            snapshot
                .entries
                .into_iter()
                .map(|(owner, entry)| (owner, entry.descriptor.id))
                .collect::<BTreeMap<_, _>>(),
            owners.clone()
        );
        prop_assert_eq!(
            actual
                .entries_for_owners((0..OWNERS + 2).map(owner))
                .unwrap()
                .into_iter()
                .map(|(owner, entry)| (owner, entry.descriptor.id))
                .collect::<BTreeMap<_, _>>(),
            owners
        );
        prop_assert_eq!(
            actual
                .native_requirements_from_roots(
                    &ids.iter()
                        .copied()
                        .map(NativeRequirementRoot::AllGroups)
                        .collect::<Vec<_>>(),
                )
                .unwrap(),
            NativeRequirements::default()
        );
        for (other_slot, other) in model.views.iter().enumerate().take(slot) {
            if let Some(other) = other {
                let other_descriptors: BTreeMap<_, _> = model
                    .contents(other)
                    .into_iter()
                    .map(|id| (id, model.records[other.inventory][&id].clone()))
                    .collect();
                let these: BTreeMap<_, _> =
                    ids.iter().map(|id| (*id, records[id].clone())).collect();
                prop_assert_eq!(
                    actual == views[other_slot].as_ref().unwrap(),
                    these == other_descriptors
                );
            }
        }
    }
    Ok(())
}

fn run_history(
    catalog: &Catalog,
    operations: &[Op],
    coverage: &mut Coverage,
) -> Result<(), TestCaseError> {
    let inventories = [ArtifactInventory::default(), ArtifactInventory::default()];
    let mut views: [Option<ArtifactView>; SLOTS] = Default::default();
    let mut model = Model::default();
    for inventory in 0..2 {
        views[inventory] = Some(inventories[inventory].empty_view());
        model.views[inventory] = Some(View {
            inventory,
            roots: BTreeSet::new(),
        });
    }
    for (step, op) in operations.iter().enumerate() {
        let replacement: Option<(usize, View, ArtifactView)> = match op {
            Op::Empty { inventory, to } => Some((
                *to,
                View {
                    inventory: *inventory,
                    roots: BTreeSet::new(),
                },
                inventories[*inventory].empty_view(),
            )),
            Op::Drop(slot) => {
                views[*slot] = None;
                model.views[*slot] = None;
                None
            }
            Op::Clone { from, to } => {
                if let (Some(expected), Some(actual)) = (&model.views[*from], &views[*from]) {
                    coverage.cloned += 1;
                    Some((*to, expected.clone(), actual.clone()))
                } else {
                    None
                }
            }
            Op::Admit {
                inventory,
                parent,
                entries,
                to,
            } => {
                if let (Some(parent_model), Some(parent_view)) =
                    (model.views[*parent].clone(), &views[*parent])
                {
                    let incoming: Vec<_> = entries
                        .iter()
                        .map(|i| {
                            (
                                catalog.specs[*i].clone(),
                                catalog.entries[*i].descriptor.clone(),
                            )
                        })
                        .collect();
                    let reused = incoming
                        .iter()
                        .any(|(_, d)| model.records[*inventory].contains_key(&d.id));
                    let expected = model.admission(*inventory, &parent_model, &incoming);
                    let actual = inventories[*inventory].admit(
                        parent_view,
                        entries
                            .iter()
                            .map(|i| catalog.entries[*i].clone())
                            .collect(),
                    );
                    match (expected, actual) {
                        (Ok(expected), Ok(actual)) => {
                            coverage.admitted += 1;
                            coverage.reused += usize::from(reused);
                            Some((*to, expected, actual))
                        }
                        (Err(expected), Err(actual)) => {
                            check_refusal(expected, actual, coverage)?;
                            None
                        }
                        (expected, actual) => {
                            return Err(TestCaseError::fail(format!(
                            "step {step} {op:?}: admission expected {expected:?}, got {actual:?}"
                        )))
                        }
                    }
                } else {
                    None
                }
            }
            Op::Select {
                from,
                roots,
                outside,
                to,
            } => {
                if let (Some(expected), Some(actual)) = (&model.views[*from], &views[*from]) {
                    let mut roots: Vec<_> = roots
                        .iter()
                        .map(|i| catalog.entries[*i].descriptor.id)
                        .collect();
                    if *outside {
                        roots.push(ArtifactId([255; 32]));
                    }
                    let owned = model.contents(expected);
                    let valid = roots.iter().all(|id| owned.contains(id));
                    let selected = actual.select_roots(roots.clone());
                    prop_assert_eq!(selected.is_ok(), valid, "step {} {:?}", step, op);
                    let native_roots: Vec<_> = roots
                        .iter()
                        .copied()
                        .map(NativeRequirementRoot::AllGroups)
                        .collect();
                    let native = actual.native_requirements_from_roots(&native_roots);
                    prop_assert_eq!(native.is_ok(), valid);
                    if valid {
                        prop_assert_eq!(native.unwrap(), NativeRequirements::default());
                        coverage.selected += 1;
                        Some((
                            *to,
                            View {
                                inventory: expected.inventory,
                                roots: roots.into_iter().collect(),
                            },
                            selected.unwrap(),
                        ))
                    } else {
                        coverage.outside_refused += 1;
                        None
                    }
                } else {
                    None
                }
            }
            Op::Project { from, owners, to } => {
                if let (Some(expected), Some(actual)) = (&model.views[*from], &views[*from]) {
                    let available: BTreeMap<_, _> = model
                        .contents(expected)
                        .into_iter()
                        .map(|id| (model.records[expected.inventory][&id].spec.key, id))
                        .collect();
                    let valid = owners.iter().all(|key| available.contains_key(key));
                    let projected = actual.interface_projection(
                        &owners.iter().map(|key| owner(*key)).collect::<Vec<_>>(),
                    );
                    prop_assert_eq!(projected.is_ok(), valid, "step {} {:?}", step, op);
                    if valid {
                        coverage.selected += 1;
                        Some((
                            *to,
                            View {
                                inventory: expected.inventory,
                                roots: owners.iter().map(|key| available[key]).collect(),
                            },
                            projected.unwrap(),
                        ))
                    } else {
                        coverage.outside_refused += 1;
                        None
                    }
                } else {
                    None
                }
            }
            Op::Merge { left, right, to } => {
                if let (Some(left_model), Some(right_model), Some(left_view), Some(right_view)) = (
                    model.views[*left].clone(),
                    model.views[*right].clone(),
                    &views[*left],
                    &views[*right],
                ) {
                    let expected = if right_model.roots.is_empty() {
                        Ok(left_model.clone())
                    } else if left_model.roots.is_empty() {
                        Ok(right_model.clone())
                    } else {
                        let incoming: Vec<_> = model
                            .contents(&right_model)
                            .iter()
                            .map(|id| {
                                let record = &model.records[right_model.inventory][id];
                                (record.spec.clone(), record.descriptor.clone())
                            })
                            .collect();
                        model
                            .admission(left_model.inventory, &left_model, &incoming)
                            .map(|_| View {
                                inventory: left_model.inventory,
                                roots: left_model
                                    .roots
                                    .union(&right_model.roots)
                                    .copied()
                                    .collect(),
                            })
                    };
                    let actual = left_view.merge(right_view);
                    match (expected, actual) {
                        (Ok(expected), Ok(actual)) => {
                            coverage.merged += 1;
                            coverage.cross_merged +=
                                usize::from(left_model.inventory != right_model.inventory);
                            Some((*to, expected, actual))
                        }
                        (Err(expected), Err(actual)) => {
                            check_refusal(expected, actual, coverage)?;
                            None
                        }
                        (expected, actual) => {
                            return Err(TestCaseError::fail(format!(
                                "step {step} {op:?}: merge expected {expected:?}, got {actual:?}"
                            )))
                        }
                    }
                } else {
                    None
                }
            }
        };
        if let Some((to, expected, actual)) = replacement {
            model.views[to] = Some(expected);
            views[to] = Some(actual);
        }
        coverage.reclaimed += model.reclaim();
        observe(&model, &inventories, &views)
            .map_err(|error| TestCaseError::fail(format!("step {step} {op:?}: {error}")))?;
    }
    // Complete release is part of every history, including one whose generated
    // tail never chooses a drop. Empty inventories can then reuse exact IDs.
    views = Default::default();
    model.views = Default::default();
    coverage.reclaimed += model.reclaim();
    observe(&model, &inventories, &views)?;
    let readmitted = inventories[0]
        .admit(
            &inventories[0].empty_view(),
            catalog.entries[..OWNERS].to_vec(),
        )
        .unwrap();
    prop_assert_eq!(
        readmitted
            .artifact_ids()
            .into_iter()
            .collect::<BTreeSet<_>>(),
        catalog.entries[..OWNERS]
            .iter()
            .map(|e| e.descriptor.id)
            .collect()
    );
    drop(readmitted);
    prop_assert_eq!(inventories[0].node_count(), 0);
    Ok(())
}

fn prefix() -> Vec<Op> {
    vec![
        Op::Admit {
            inventory: 0,
            parent: 0,
            entries: (0..OWNERS).collect(),
            to: 2,
        },
        Op::Admit {
            inventory: 1,
            parent: 1,
            entries: (0..OWNERS).rev().collect(),
            to: 3,
        },
        Op::Admit {
            inventory: 0,
            parent: 0,
            entries: vec![2, 2],
            to: 4,
        },
        Op::Admit {
            inventory: 0,
            parent: 2,
            entries: vec![2 * OWNERS],
            to: 4,
        },
        Op::Admit {
            inventory: 0,
            parent: 4,
            entries: vec![2 * OWNERS + 1],
            to: 7,
        },
        Op::Admit {
            inventory: 0,
            parent: 2,
            entries: vec![OWNERS],
            to: 7,
        },
        Op::Admit {
            inventory: 1,
            parent: 2,
            entries: vec![],
            to: 7,
        },
        Op::Merge {
            left: 2,
            right: 3,
            to: 5,
        },
        Op::Select {
            from: 2,
            roots: vec![0, 0],
            outside: false,
            to: 6,
        },
        Op::Select {
            from: 2,
            roots: vec![1],
            outside: false,
            to: 7,
        },
        Op::Select {
            from: 2,
            roots: vec![0],
            outside: true,
            to: 5,
        },
        Op::Clone { from: 6, to: 5 },
        Op::Drop(2),
        Op::Drop(3),
        Op::Drop(4),
        Op::Drop(6),
        Op::Project {
            from: 7,
            owners: vec![1, 1],
            to: 6,
        },
        Op::Merge {
            left: 5,
            right: 6,
            to: 4,
        },
        Op::Select {
            from: 4,
            roots: vec![],
            outside: false,
            to: 2,
        },
    ]
}

fn property_config() -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 96;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn retained_histories_match_independent_set_model(
        masks in proptest::collection::vec(0u8..64, OWNERS), tail in operations(),
    ) {
        let catalog = Catalog::new(&masks);
        let mut history = prefix();
        history.extend(tail);
        run_history(&catalog, &history, &mut Coverage::default())?;
    }
}

#[test]
fn targeted_histories_exercise_admission_selection_and_shared_reclamation() {
    // Shared target, a cycle, same module spellings in different units, and an
    // unrelated leaf. Coverage records executed outcomes, never intended ops.
    let catalog = Catalog::new(&[4, 4, 0, 16, 8, 0]);
    let mut history = prefix();
    history.extend([
        Op::Drop(4),
        Op::Drop(5),
        Op::Drop(6),
        Op::Drop(7),
        Op::Admit {
            inventory: 0,
            parent: 0,
            entries: vec![0],
            to: 2,
        },
        Op::Admit {
            inventory: 0,
            parent: 0,
            entries: vec![OWNERS + 2],
            to: 3,
        },
        Op::Admit {
            inventory: 0,
            parent: 3,
            entries: vec![0],
            to: 2,
        },
    ]);
    let mut coverage = Coverage::default();
    run_history(&catalog, &history, &mut coverage).unwrap();
    assert!(coverage.admitted > 0 && coverage.reused > 0, "{coverage:?}");
    assert!(
        coverage.metadata_refused > 0 && coverage.owner_refused > 0,
        "{coverage:?}"
    );
    assert!(
        coverage.dependency_refused > 0 && coverage.seal_refused > 0,
        "{coverage:?}"
    );
    assert!(
        coverage.foreign_refused > 0 && coverage.outside_refused > 0,
        "{coverage:?}"
    );
    assert!(coverage.selected > 0 && coverage.cloned > 0, "{coverage:?}");
    assert!(
        coverage.merged > 0 && coverage.cross_merged > 0 && coverage.reclaimed > 0,
        "{coverage:?}"
    );
    eprintln!("artifact inventory history coverage: {coverage:?}");
}

fn native_fixture(
    name: &str,
    import: Option<crate::certified_products::PendingImportOwner>,
    version: u8,
) -> ArtifactEntry {
    let product = crate::certified_products::tests::original_witness_fixture(
        name,
        import,
        version,
        &BTreeMap::new(),
    );
    ArtifactEntry::original(
        [2; 32],
        crate::certified_products::fixture_finalized_product(product, [2; 32]),
    )
    .unwrap()
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn native_roots_preserve_binding_generations_and_type_isolation(
        generation in 0u64..4,
        version in 1u8..4,
        root_mask in 0u8..16,
        duplicate in any::<bool>(),
    ) {
        use crate::certified_products::PendingImportOwner;
        use tidepool_repr::execution_schema::SymbolIdentity;
        let value_owner = ExactModuleIdentity { unit: "fixture".into(), module: "Value".into() };
        let value = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32], &value_owner.unit, &value_owner.module, BTreeMap::new(),
        ));
        let binding = SymbolIdentity {
            unit: value_owner.unit.clone(), module: value_owner.module.clone(),
            namespace: "value".into(), occurrence: "x".into(), record_parent: None,
        };
        let helper = native_fixture("Helper", Some(PendingImportOwner::Retained { identity: binding.clone(), generation }), version);
        let ArtifactPayload::Original(helper_product) = &helper.payload else { unreachable!() };
        let root = native_fixture("Root", Some(PendingImportOwner::Source {
            owner: helper_product.owner().clone(), original_ordinal: 7,
            binder: SymbolIdentity { unit: "fixture".into(), module: "Helper".into(), namespace: "value".into(), occurrence: "entry".into(), record_parent: None },
        }), 1);
        let type_only = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32], "fixture", "TypeOnly", BTreeMap::from([((value_owner.unit.clone(), value_owner.module.clone()), value.descriptor.interface_sha256)]),
        ));
        let choices = [root.descriptor.id, helper.descriptor.id, type_only.descriptor.id, value.descriptor.id];
        let mut roots: Vec<_> = choices.iter().enumerate().filter(|(i, _)| root_mask & (1 << i) != 0).map(|(_, id)| *id).collect();
        if duplicate { roots.extend(roots.clone()); }
        // Independent truth table: every root selects all its own groups. A
        // source edge selects Helper's issued group 7, which requires this
        // binding. Interface-only roots never demand a native binding.
        let demanded = root_mask & 3 != 0;
        let expected = if demanded {
            vec![NativeBindingRequirement { artifact_id: value.descriptor.id, identity: binding, generation }]
        } else { Vec::new() };
        let inventory = ArtifactInventory::default();
        let admitted = inventory.admit(&inventory.empty_view(), vec![root, helper, type_only, value]).unwrap();
        let selected = admitted.select_roots(roots.clone()).unwrap();
        let captured = selected.clone();
        drop(admitted);
        drop(selected);
        let native_roots: Vec<_> = roots
            .iter()
            .copied()
            .map(NativeRequirementRoot::AllGroups)
            .collect();
        let requirements = captured.native_requirements_from_roots(&native_roots).unwrap();
        prop_assert_eq!(&requirements.bindings, &expected);
        prop_assert!(requirements.packages.is_empty());
        prop_assert_eq!(captured.native_binding_requirements_from_roots(&native_roots).unwrap(), expected);
        prop_assert!(captured.native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(ArtifactId([255; 32]))]).is_err());
        drop(captured);
        prop_assert_eq!(inventory.node_count(), 0);
    }
}

const GROUP_ORDINALS: [u32; 2] = [7, 11];
const GROUP_NODES: usize = 4;

type GroupState = (usize, u32);

struct GroupGraphFixture {
    view: ArtifactView,
    reversed_view: ArtifactView,
    binding_ids: Vec<ArtifactId>,
    native_ids: Vec<ArtifactId>,
    root_id: ArtifactId,
}

fn group_graph_fixture(extra_edges: &[u8], generations: &[u64]) -> GroupGraphFixture {
    use crate::certified_products::{tests::original_groups_fixture, PendingImportOwner};
    use tidepool_repr::execution_schema::SymbolIdentity;

    let empty_groups: Vec<(u32, Vec<PendingImportOwner>)> = GROUP_ORDINALS
        .iter()
        .map(|ordinal| (*ordinal, Vec::new()))
        .collect();
    let placeholders: Vec<_> = (0..GROUP_NODES)
        .map(|node| {
            original_groups_fixture(
                &format!("Node{node}"),
                empty_groups.clone(),
                1,
                &BTreeMap::new(),
            )
        })
        .collect();
    let owners: Vec<_> = placeholders
        .iter()
        .map(|product| product.owner().clone())
        .collect();

    // This core is a diamond with a cycle: 0/7 -> {1/11, 2/11} -> 3/7
    // -> 1/11. Node 0's other group is deliberately outside that closure.
    let mut edges = BTreeMap::<GroupState, BTreeSet<GroupState>>::new();
    for (from, to) in [
        ((0, 7), (1, 11)),
        ((0, 7), (2, 11)),
        ((1, 11), (3, 7)),
        ((2, 11), (3, 7)),
        ((3, 7), (1, 11)),
        ((0, 11), (3, 11)),
    ] {
        edges.entry(from).or_default().insert(to);
    }
    for node in 0..GROUP_NODES {
        for (ordinal_index, ordinal) in GROUP_ORDINALS.iter().copied().enumerate() {
            let mask = extra_edges[node * GROUP_ORDINALS.len() + ordinal_index];
            for target in 1..GROUP_NODES {
                if target != node && mask & (1 << target) != 0 {
                    let required =
                        GROUP_ORDINALS[(node + target + ordinal_index) % GROUP_ORDINALS.len()];
                    edges
                        .entry((node, ordinal))
                        .or_default()
                        .insert((target, required));
                }
            }
        }
    }

    let build_products = |owners: &[tidepool_repr::execution_schema::CachedHomeOwner]| {
        (0..GROUP_NODES)
            .map(|node| {
                let groups = GROUP_ORDINALS
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(ordinal_index, ordinal)| {
                        let mut imports = Vec::new();
                        for (target, required_ordinal) in
                            edges.get(&(node, ordinal)).into_iter().flatten().copied()
                        {
                            let target_module = &owners[target].module;
                            imports.push(PendingImportOwner::Source {
                                owner: owners[target].clone(),
                                original_ordinal: required_ordinal,
                                binder: SymbolIdentity {
                                    unit: owners[target].unit.clone(),
                                    module: target_module.clone(),
                                    namespace: "value".into(),
                                    occurrence: format!("entry_{required_ordinal}"),
                                    record_parent: None,
                                },
                            });
                        }
                        imports.push(PendingImportOwner::Retained {
                            identity: SymbolIdentity {
                                unit: "fixture".into(),
                                module: format!("Value{node}"),
                                namespace: "value".into(),
                                occurrence: format!("binding_{ordinal}"),
                                record_parent: None,
                            },
                            generation: generations[node * GROUP_ORDINALS.len() + ordinal_index],
                        });
                        (ordinal, imports)
                    })
                    .collect();
                original_groups_fixture(&format!("Node{node}"), groups, 1, &BTreeMap::new())
            })
            .collect::<Vec<_>>()
    };
    let first_pass = build_products(&owners);
    let exact_owners: Vec<_> = first_pass
        .iter()
        .map(|product| product.owner().clone())
        .collect();
    let products = build_products(&exact_owners);
    for (first, second) in first_pass.iter().zip(&products) {
        assert_eq!(first.owner(), second.owner());
    }

    let mut entries: Vec<_> = products
        .into_iter()
        .map(|product| {
            ArtifactEntry::original(
                [2; 32],
                crate::certified_products::fixture_finalized_product(product, [2; 32]),
            )
            .unwrap()
        })
        .collect();
    let native_ids = entries.iter().map(|entry| entry.descriptor.id).collect();
    let value_ids: Vec<_> = (0..GROUP_NODES)
        .map(|node| {
            let owner = ExactModuleIdentity {
                unit: "fixture".into(),
                module: format!("Value{node}"),
            };
            let entry =
                ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                    [2; 32],
                    &owner.unit,
                    &owner.module,
                    BTreeMap::new(),
                ));
            let id = entry.descriptor.id;
            entries.push(entry);
            id
        })
        .collect();
    // Carry the exact native identity before admission adds a canonical module
    // with the same owner. Demand must never select by module name alone.
    assert_eq!(entries[0].descriptor.kind, ArtifactKind::OriginalModule);
    assert_eq!(entries[0].descriptor.owner.module, "Node0");
    let root_id = entries[0].descriptor.id;
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit(&inventory.empty_view(), entries.clone())
        .unwrap();
    entries.reverse();
    let reversed_inventory = ArtifactInventory::default();
    let reversed_view = reversed_inventory
        .admit(&reversed_inventory.empty_view(), entries)
        .unwrap();
    GroupGraphFixture {
        view,
        reversed_view,
        binding_ids: value_ids,
        native_ids,
        root_id,
    }
}

#[test]
fn native_group_fixture_distinguishes_original_and_canonical_owner_in_both_orders() {
    let GroupGraphFixture {
        view,
        reversed_view,
        root_id,
        ..
    } = group_graph_fixture(
        &[0; GROUP_NODES * GROUP_ORDINALS.len()],
        &[0; GROUP_NODES * GROUP_ORDINALS.len()],
    );
    for view in [&view, &reversed_view] {
        let owner_artifacts: Vec<_> = view
            .descriptors()
            .into_iter()
            .filter(|descriptor| {
                descriptor.owner
                    == ExactModuleIdentity {
                        unit: "fixture".into(),
                        module: "Node0".into(),
                    }
            })
            .collect();
        assert_eq!(owner_artifacts.len(), 2);
        assert!(owner_artifacts
            .iter()
            .any(|descriptor| descriptor.id == root_id
                && descriptor.kind == ArtifactKind::OriginalModule));
        let canonical: Vec<_> = owner_artifacts
            .iter()
            .filter(|descriptor| descriptor.kind == ArtifactKind::CanonicalModuleInterface)
            .collect();
        assert_eq!(canonical.len(), 1);
        let canonical_id = canonical[0].id;
        assert_ne!(root_id, canonical_id);
        let native = view
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: root_id,
                original_ordinal: 7,
            }])
            .unwrap();
        assert_eq!(native.bindings.len(), 4);
        assert!(
            matches!(view.native_requirements_from_roots(&[NativeRequirementRoot::Group { artifact: canonical_id, original_ordinal: 7 }]),
            Err(CompileError::ArtifactInventory(error)) if error.failure == ArtifactInventoryFailure::NativeGroupUnavailable { artifact: canonical_id, original_ordinal: 7 })
        );
    }
}

// Repeated set expansion is intentionally independent of the production
// worklist and its graph indices.
fn selected_group_closure(
    edges: &BTreeMap<GroupState, BTreeSet<GroupState>>,
    roots: &BTreeSet<GroupState>,
) -> BTreeSet<GroupState> {
    let mut selected = roots.clone();
    loop {
        let before = selected.len();
        let newly_required: Vec<_> = selected
            .iter()
            .flat_map(|state| edges.get(state).into_iter().flatten().copied())
            .collect();
        selected.extend(newly_required);
        if selected.len() == before {
            return selected;
        }
    }
}

fn bindings_for_selected_groups(
    groups: &BTreeSet<GroupState>,
    binding_ids: &[ArtifactId],
    generations: &[u64],
) -> BTreeSet<NativeBindingRequirement> {
    use tidepool_repr::execution_schema::SymbolIdentity;

    groups
        .iter()
        .map(|(node, ordinal)| {
            let ordinal_index = GROUP_ORDINALS
                .iter()
                .position(|candidate| candidate == ordinal)
                .unwrap();
            NativeBindingRequirement {
                artifact_id: binding_ids[*node],
                identity: SymbolIdentity {
                    unit: "fixture".into(),
                    module: format!("Value{node}"),
                    namespace: "value".into(),
                    occurrence: format!("binding_{ordinal}"),
                    record_parent: None,
                },
                generation: generations[node * GROUP_ORDINALS.len() + ordinal_index],
            }
        })
        .collect()
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn selected_native_groups_match_independent_fixed_point(
        extra_edges in proptest::collection::vec(0u8..16, GROUP_NODES * GROUP_ORDINALS.len()),
        generations in proptest::collection::vec(0u64..8, GROUP_NODES * GROUP_ORDINALS.len()),
    ) {
        let generations: Vec<_> = generations
            .into_iter()
            .enumerate()
            .map(|(index, generation)| generation + index as u64 * 16)
            .collect();
        let mut edges = BTreeMap::<GroupState, BTreeSet<GroupState>>::new();
        for (from, to) in [
            ((0, 7), (1, 11)),
            ((0, 7), (2, 11)),
            ((1, 11), (3, 7)),
            ((2, 11), (3, 7)),
            ((3, 7), (1, 11)),
            ((0, 11), (3, 11)),
        ] {
            edges.entry(from).or_default().insert(to);
        }
        for node in 0..GROUP_NODES {
            for (ordinal_index, ordinal) in GROUP_ORDINALS.iter().copied().enumerate() {
                let mask = extra_edges[node * GROUP_ORDINALS.len() + ordinal_index];
                for target in 1..GROUP_NODES {
                    if target != node && mask & (1 << target) != 0 {
                        let required =
                            GROUP_ORDINALS[(node + target + ordinal_index) % GROUP_ORDINALS.len()];
                        edges.entry((node, ordinal)).or_default().insert((target, required));
                    }
                }
            }
        }
        let GroupGraphFixture { view, reversed_view, binding_ids, native_ids, root_id } = group_graph_fixture(&extra_edges, &generations);
        let roots = [NativeRequirementRoot::Group {
            artifact: root_id,
            original_ordinal: 7,
        }];
        let expected_groups = selected_group_closure(&edges, &BTreeSet::from([(0, 7)]));
        let expected_bindings =
            bindings_for_selected_groups(&expected_groups, &binding_ids, &generations);
        let (actual, actual_groups) = view.native_requirements_with_groups_from_roots(&roots).unwrap();
        let expected_keys = expected_groups.iter().map(|(node, ordinal)| NativeGroupKey {
            artifact: native_ids[*node],
            original_ordinal: *ordinal,
        }).collect::<BTreeSet<_>>();
        prop_assert_eq!(&actual_groups, &expected_keys);
        prop_assert_eq!(&actual_groups, &reversed_view.native_requirements_with_groups_from_roots(&roots).unwrap().1);
        prop_assert!(actual_groups.len() < view.selected_native_groups().len(), "available groups must not become executable roots");
        prop_assert_eq!(&actual, &reversed_view.native_requirements_from_roots(&roots).unwrap());
        prop_assert_eq!(
            actual.bindings.iter().cloned().collect::<BTreeSet<_>>(),
            expected_bindings
        );
        prop_assert!(actual.packages.is_empty());
        prop_assert!(!expected_groups.is_empty());
        prop_assert!(!expected_groups.contains(&(0, 11)), "the unselected later group on the root became a demand");

        let all_groups = [NativeRequirementRoot::AllGroups(root_id)];
        let expected_all = selected_group_closure(
            &edges,
            &BTreeSet::from([(0, 7), (0, 11)]),
        );
        let (all_requirements, all_selected) = view.native_requirements_with_groups_from_roots(&all_groups).unwrap();
        prop_assert_eq!(all_selected, expected_all.iter().map(|(node, ordinal)| NativeGroupKey {
            artifact: native_ids[*node],
            original_ordinal: *ordinal,
        }).collect::<BTreeSet<_>>());
        prop_assert_eq!(&all_requirements, &reversed_view.native_requirements_from_roots(&all_groups).unwrap());
        prop_assert_eq!(
            all_requirements.bindings.iter().cloned().collect::<BTreeSet<_>>(),
            bindings_for_selected_groups(&expected_all, &binding_ids, &generations)
        );
        prop_assert!(expected_all.contains(&(0, 11)));
    }
}
