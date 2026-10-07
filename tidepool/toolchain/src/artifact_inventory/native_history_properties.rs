//! Staged native admission is compared with raw fixture facts, not inventory
//! edges. The reference expands finite sets from scratch after every operation.
use super::*;
use crate::certified_products::{tests::original_groups_fixture, PendingImportOwner};
use proptest::prelude::*;
use proptest::test_runner::{FileFailurePersistence, TestCaseError};
use tidepool_repr::execution_schema::{CachedHomeOwner, SymbolIdentity};

const MODULES: usize = 4;
const SLOTS: usize = 8;
const ORDINALS: [u32; 3] = [3, 11, 29];
type RawGroup = (usize, u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Fact {
    Carrier(ArtifactId),
    Group(RawGroup),
}

struct Catalog {
    originals: Vec<Arc<ArtifactEntry>>,
    interfaces: Vec<Arc<ArtifactEntry>>,
    values: [Arc<ArtifactEntry>; 2],
    edges: BTreeMap<RawGroup, BTreeSet<RawGroup>>,
    bindings: BTreeMap<RawGroup, (usize, u64)>,
}

impl Catalog {
    fn new(masks: &[u8], generations: &[u64], version: u8) -> Self {
        // The early diamond shares a cyclic helper. Only ordinal 11 needs the
        // future value; unused ordinal 29 has no native dependency at all.
        let mut edges = BTreeMap::<RawGroup, BTreeSet<RawGroup>>::new();
        for (from, to) in [
            ((0, 3), (1, 3)),
            ((0, 3), (2, 3)),
            ((1, 3), (3, 3)),
            ((2, 3), (3, 3)),
            ((3, 3), (1, 3)),
            ((0, 11), (1, 11)),
            ((1, 11), (3, 3)),
        ] {
            edges.entry(from).or_default().insert(to);
        }
        for (node, mask) in masks.iter().copied().enumerate() {
            for target in 0..MODULES {
                if target != node && mask & (1 << target) != 0 {
                    edges.entry((node, 3)).or_default().insert((target, 3));
                }
            }
        }
        let bindings = (0..MODULES)
            .flat_map(|node| {
                [
                    ((node, 3), (0, generations[node])),
                    ((node, 11), (1, generations[MODULES + node])),
                ]
            })
            .collect::<BTreeMap<_, _>>();
        let placeholders = (0..MODULES)
            .map(|node| {
                original_groups_fixture(
                    &format!("History{node}"),
                    ORDINALS
                        .iter()
                        .map(|ordinal| (*ordinal, Vec::new()))
                        .collect(),
                    version,
                    &BTreeMap::new(),
                )
            })
            .collect::<Vec<_>>();
        let build = |owners: &[CachedHomeOwner]| {
            (0..MODULES)
                .map(|node| {
                    let groups = ORDINALS
                        .iter()
                        .copied()
                        .map(|ordinal| {
                            let group = (node, ordinal);
                            let mut imports = edges
                                .get(&group)
                                .into_iter()
                                .flatten()
                                .map(|(target, required)| PendingImportOwner::Source {
                                    owner: owners[*target].clone(),
                                    original_ordinal: *required,
                                    binder: SymbolIdentity {
                                        unit: "fixture".into(),
                                        module: format!("History{target}"),
                                        namespace: "value".into(),
                                        occurrence: format!("entry_{required}"),
                                        record_parent: None,
                                    },
                                })
                                .collect::<Vec<_>>();
                            if let Some((value, generation)) = bindings.get(&group) {
                                imports.push(PendingImportOwner::Retained {
                                    identity: binding_identity(*value, group),
                                    generation: *generation,
                                });
                            }
                            (ordinal, imports)
                        })
                        .collect();
                    original_groups_fixture(
                        &format!("History{node}"),
                        groups,
                        version,
                        &BTreeMap::new(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let first = build(
            &placeholders
                .iter()
                .map(|p| p.owner().clone())
                .collect::<Vec<_>>(),
        );
        let products = build(&first.iter().map(|p| p.owner().clone()).collect::<Vec<_>>());
        for (left, right) in first.iter().zip(&products) {
            assert_eq!(
                left.owner(),
                right.owner(),
                "exact source owner fixed point"
            );
        }
        let finalized = products
            .into_iter()
            .map(|product| crate::certified_products::fixture_finalized_product(product, [2; 32]))
            .collect::<Vec<_>>();
        let interfaces = finalized
            .iter()
            .map(|product| {
                Arc::new(ArtifactEntry::canonical(
                    product.module_interface().unwrap().clone(),
                ))
            })
            .collect();
        let originals = finalized
            .into_iter()
            .map(|product| Arc::new(ArtifactEntry::original([2; 32], product).unwrap()))
            .collect();
        let values = ["BaselineValue", "FutureValue"].map(|module| {
            Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    [2; 32],
                    "fixture",
                    module,
                    BTreeMap::new(),
                ),
            ))
        });
        Self {
            originals,
            interfaces,
            values,
            edges,
            bindings,
        }
    }

    fn key(&self, group: RawGroup) -> NativeGroupKey {
        NativeGroupKey {
            artifact: self.originals[group.0].descriptor.id,
            original_ordinal: group.1,
        }
    }

    fn group_closure(&self, roots: &BTreeSet<RawGroup>) -> BTreeSet<RawGroup> {
        let mut groups = roots.clone();
        loop {
            let prior = groups.clone();
            for group in &prior {
                groups.extend(self.edges.get(group).into_iter().flatten().copied());
            }
            if prior == groups {
                return groups;
            }
        }
    }

    fn facts(&self, carriers: BTreeSet<ArtifactId>, groups: BTreeSet<RawGroup>) -> BTreeSet<Fact> {
        let mut facts = carriers
            .into_iter()
            .map(Fact::Carrier)
            .collect::<BTreeSet<_>>();
        facts.extend(groups.into_iter().map(Fact::Group));
        loop {
            let prior = facts.clone();
            for fact in &prior {
                match *fact {
                    Fact::Carrier(id) => {
                        for (node, original) in self.originals.iter().enumerate() {
                            if id == original.descriptor.id {
                                facts.insert(Fact::Carrier(self.interfaces[node].descriptor.id));
                            }
                        }
                    }
                    Fact::Group(group) => {
                        facts.insert(Fact::Carrier(self.originals[group.0].descriptor.id));
                        facts.extend(
                            self.edges
                                .get(&group)
                                .into_iter()
                                .flatten()
                                .copied()
                                .map(Fact::Group),
                        );
                        if let Some((value, _)) = self.bindings.get(&group) {
                            facts.insert(Fact::Carrier(self.values[*value].descriptor.id));
                        }
                    }
                }
            }
            if facts == prior {
                return facts;
            }
        }
    }

    fn entries(&self, future: bool, reverse: bool) -> Vec<Arc<ArtifactEntry>> {
        let mut entries = self.originals.clone();
        entries.push(self.values[0].clone());
        if future {
            entries.push(self.values[1].clone());
        }
        if reverse {
            entries.reverse();
        }
        entries
    }

    fn supplied_ids(&self, future: bool) -> BTreeSet<ArtifactId> {
        self.originals
            .iter()
            .chain(&self.interfaces)
            .chain(self.values[..if future { 2 } else { 1 }].iter())
            .map(|entry| entry.descriptor.id)
            .collect()
    }

    fn selection(&self, groups: &BTreeSet<RawGroup>) -> BTreeSet<NativeGroupKey> {
        groups.iter().map(|group| self.key(*group)).collect()
    }

    fn bindings_for(&self, roots: &BTreeSet<RawGroup>) -> Vec<NativeBindingRequirement> {
        self.group_closure(roots)
            .iter()
            .filter_map(|group| {
                self.bindings
                    .get(group)
                    .map(|(value, generation)| NativeBindingRequirement {
                        artifact_id: self.values[*value].descriptor.id,
                        identity: binding_identity(*value, *group),
                        generation: *generation,
                    })
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

fn binding_identity(value: usize, (node, ordinal): RawGroup) -> SymbolIdentity {
    SymbolIdentity {
        unit: "fixture".into(),
        module: if value == 0 {
            "BaselineValue"
        } else {
            "FutureValue"
        }
        .into(),
        namespace: "value".into(),
        occurrence: format!("binding_{node}_{ordinal}"),
        record_parent: None,
    }
}

fn carrier_ids(facts: &BTreeSet<Fact>) -> BTreeSet<ArtifactId> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Carrier(id) => Some(*id),
            _ => None,
        })
        .collect()
}

fn groups(facts: &BTreeSet<Fact>) -> BTreeSet<RawGroup> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Group(group) => Some(*group),
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug)]
struct View {
    inventory: usize,
    facts: BTreeSet<Fact>,
}

#[derive(Clone, Debug)]
enum Op {
    Empty {
        inventory: usize,
        to: usize,
    },
    Admit {
        parent: usize,
        root: u8,
        future: bool,
        reverse: bool,
        to: usize,
    },
    Clone {
        from: usize,
        to: usize,
    },
    Drop(usize),
    Project {
        from: usize,
        nodes: Vec<usize>,
        to: usize,
    },
    Merge {
        left: usize,
        right: usize,
        to: usize,
    },
    Read {
        from: usize,
        node: usize,
        ordinal: u32,
        all: bool,
    },
}

fn root_groups(root: u8) -> BTreeSet<RawGroup> {
    match root {
        0 => BTreeSet::from([(0, 3)]),
        1 => BTreeSet::from([(0, 11)]),
        2 => BTreeSet::from([(0, 29)]),
        3 => (0..MODULES)
            .flat_map(|node| ORDINALS.map(|ordinal| (node, ordinal)))
            .collect(),
        _ => BTreeSet::new(),
    }
}

fn operations() -> impl Strategy<Value = Vec<Op>> {
    let slot = 0..SLOTS;
    proptest::collection::vec(
        prop_oneof![
            1 => (0usize..2, slot.clone()).prop_map(|(inventory, to)| Op::Empty { inventory, to }),
            5 => (slot.clone(), 0u8..5, any::<bool>(), any::<bool>(), slot.clone())
                .prop_map(|(parent, root, future, reverse, to)| Op::Admit { parent, root, future, reverse, to }),
            2 => (slot.clone(), slot.clone()).prop_map(|(from, to)| Op::Clone { from, to }),
            2 => slot.clone().prop_map(Op::Drop),
            3 => (slot.clone(), proptest::collection::vec(0..MODULES, 0..5), slot.clone())
                .prop_map(|(from, nodes, to)| Op::Project { from, nodes, to }),
            3 => (slot.clone(), slot.clone(), slot.clone()).prop_map(|(left, right, to)| Op::Merge { left, right, to }),
            4 => (slot, 0..MODULES, prop::sample::select(vec![3u32, 11, 29, 0, 4]), any::<bool>())
                .prop_map(|(from, node, ordinal, all)| Op::Read { from, node, ordinal, all }),
        ],
        8..49,
    )
}

#[derive(Debug, Default)]
struct Coverage {
    admissions: usize,
    refusals: usize,
    repeated: usize,
    clones: usize,
    projections: usize,
    merges: usize,
    cross_merges: usize,
    reads: usize,
    read_refusals: usize,
    reclaimed: usize,
    absent_slots: usize,
}

fn prefix() -> Vec<Op> {
    vec![
        Op::Empty {
            inventory: 0,
            to: 0,
        },
        Op::Empty {
            inventory: 1,
            to: 1,
        },
        Op::Admit {
            parent: 0,
            root: 0,
            future: false,
            reverse: false,
            to: 2,
        },
        Op::Clone { from: 2, to: 3 },
        Op::Admit {
            parent: 2,
            root: 0,
            future: false,
            reverse: true,
            to: 4,
        },
        Op::Read {
            from: 2,
            node: 0,
            ordinal: 3,
            all: false,
        },
        Op::Read {
            from: 2,
            node: 0,
            ordinal: 11,
            all: false,
        },
        Op::Read {
            from: 2,
            node: 0,
            ordinal: 0,
            all: true,
        },
        Op::Admit {
            parent: 2,
            root: 1,
            future: false,
            reverse: false,
            to: 5,
        },
        Op::Admit {
            parent: 2,
            root: 3,
            future: false,
            reverse: true,
            to: 5,
        },
        Op::Admit {
            parent: 2,
            root: 4,
            future: true,
            reverse: true,
            to: 5,
        },
        Op::Admit {
            parent: 5,
            root: 1,
            future: false,
            reverse: false,
            to: 6,
        },
        Op::Read {
            from: 3,
            node: 0,
            ordinal: 11,
            all: false,
        },
        Op::Read {
            from: 6,
            node: 0,
            ordinal: 11,
            all: false,
        },
        Op::Admit {
            parent: 6,
            root: 1,
            future: false,
            reverse: true,
            to: 6,
        },
        Op::Admit {
            parent: 1,
            root: 2,
            future: false,
            reverse: true,
            to: 7,
        },
        Op::Merge {
            left: 2,
            right: 7,
            to: 4,
        },
        Op::Merge {
            left: 2,
            right: 6,
            to: 5,
        },
        Op::Project {
            from: 6,
            nodes: vec![0],
            to: 6,
        },
        Op::Drop(2),
        Op::Drop(3),
        Op::Drop(4),
        Op::Drop(5),
        Op::Project {
            from: 6,
            nodes: vec![],
            to: 6,
        },
        Op::Drop(7),
        // Refill the slots after the reclamation control so generated tails
        // begin with live views in both inventories, rather than absent handles.
        Op::Admit {
            parent: 0,
            root: 0,
            future: false,
            reverse: true,
            to: 2,
        },
        Op::Clone { from: 2, to: 3 },
        Op::Admit {
            parent: 0,
            root: 1,
            future: true,
            reverse: false,
            to: 4,
        },
        Op::Admit {
            parent: 1,
            root: 2,
            future: false,
            reverse: false,
            to: 5,
        },
        Op::Merge {
            left: 2,
            right: 5,
            to: 7,
        },
    ]
}

fn run_history(catalog: &Catalog, ops: &[Op]) -> Result<Coverage, TestCaseError> {
    let inventories = [ArtifactInventory::default(), ArtifactInventory::default()];
    let mut actual: [Option<ArtifactView>; SLOTS] = std::array::from_fn(|_| None);
    let mut model: [Option<View>; SLOTS] = std::array::from_fn(|_| None);
    let mut coverage = Coverage::default();
    let mut prior_live = [0; 2];
    for (step, op) in ops.iter().enumerate() {
        match op {
            Op::Empty { inventory, to } => {
                actual[*to] = Some(inventories[*inventory].empty_view());
                model[*to] = Some(View {
                    inventory: *inventory,
                    facts: BTreeSet::new(),
                });
            }
            Op::Admit {
                parent,
                root,
                future,
                reverse,
                to,
            } => {
                if let Some(view) = model[*parent].clone() {
                    let mut available = carrier_ids(&view.facts);
                    available.extend(catalog.supplied_ids(*future));
                    let demanded = catalog.group_closure(&root_groups(*root));
                    let wanted = catalog.facts(available.clone(), demanded.clone());
                    let expected = carrier_ids(&wanted).is_subset(&available);
                    let before = inventories[view.inventory].node_count();
                    let result = inventories[view.inventory].admit_recovery_selection(
                        actual[*parent].as_ref().unwrap(),
                        catalog.entries(*future, *reverse),
                        &catalog.selection(&demanded),
                    );
                    prop_assert_eq!(result.is_ok(), expected, "step {}: {:?}", step, op);
                    if let Ok(result) = result {
                        let mut facts = wanted;
                        facts.extend(view.facts.clone());
                        if facts == view.facts {
                            coverage.repeated += 1;
                        }
                        actual[*to] = Some(result);
                        model[*to] = Some(View {
                            inventory: view.inventory,
                            facts,
                        });
                        coverage.admissions += 1;
                    } else {
                        prop_assert_eq!(
                            inventories[view.inventory].node_count(),
                            before,
                            "refusal must be atomic"
                        );
                        coverage.refusals += 1;
                    }
                } else {
                    coverage.absent_slots += 1;
                }
            }
            Op::Clone { from, to } => {
                actual[*to] = actual[*from].clone();
                model[*to] = model[*from].clone();
                coverage.clones += 1;
            }
            Op::Drop(slot) => {
                actual[*slot] = None;
                model[*slot] = None;
            }
            Op::Project { from, nodes, to } => {
                if let Some(view) = model[*from].clone() {
                    let ids = nodes
                        .iter()
                        .map(|node| catalog.originals[*node].descriptor.id)
                        .collect::<BTreeSet<_>>();
                    let expected = ids.is_subset(&carrier_ids(&view.facts));
                    let result = actual[*from]
                        .as_ref()
                        .unwrap()
                        .select_roots(ids.iter().copied().collect());
                    prop_assert_eq!(result.is_ok(), expected, "step {}: {:?}", step, op);
                    if let Ok(result) = result {
                        let selected = groups(&view.facts)
                            .into_iter()
                            .filter(|group| nodes.contains(&group.0))
                            .collect();
                        let facts = catalog.facts(ids, selected);
                        actual[*to] = Some(result);
                        model[*to] = Some(View {
                            inventory: view.inventory,
                            facts,
                        });
                        coverage.projections += 1;
                    } else {
                        coverage.refusals += 1;
                    }
                } else {
                    coverage.absent_slots += 1;
                }
            }
            Op::Merge { left, right, to } => {
                if let (Some(left_view), Some(right_view)) =
                    (model[*left].clone(), model[*right].clone())
                {
                    let result = actual[*left]
                        .as_ref()
                        .unwrap()
                        .merge(actual[*right].as_ref().unwrap());
                    prop_assert!(result.is_ok(), "step {}: {:?}: {:?}", step, op, result);
                    let inventory = if right_view.facts.is_empty() {
                        left_view.inventory
                    } else if left_view.facts.is_empty() {
                        right_view.inventory
                    } else {
                        left_view.inventory
                    };
                    let mut facts = left_view.facts;
                    facts.extend(right_view.facts);
                    actual[*to] = Some(result.unwrap());
                    model[*to] = Some(View { inventory, facts });
                    coverage.merges += 1;
                    if left_view.inventory != right_view.inventory {
                        coverage.cross_merges += 1;
                    }
                } else {
                    coverage.absent_slots += 1;
                }
            }
            Op::Read {
                from,
                node,
                ordinal,
                all,
            } => {
                if let Some(view) = &model[*from] {
                    let roots = if *all {
                        ORDINALS
                            .map(|ordinal| (*node, ordinal))
                            .into_iter()
                            .collect()
                    } else {
                        BTreeSet::from([(*node, *ordinal)])
                    };
                    let expected = roots.is_subset(&groups(&view.facts))
                        && carrier_ids(&view.facts)
                            .contains(&catalog.originals[*node].descriptor.id);
                    let root = if *all {
                        NativeRequirementRoot::AllGroups(catalog.originals[*node].descriptor.id)
                    } else {
                        NativeRequirementRoot::Group {
                            artifact: catalog.originals[*node].descriptor.id,
                            original_ordinal: *ordinal,
                        }
                    };
                    let result = actual[*from]
                        .as_ref()
                        .unwrap()
                        .native_requirements_from_roots(&[root]);
                    prop_assert_eq!(result.is_ok(), expected, "step {}: {:?}", step, op);
                    if let Ok(result) = result {
                        prop_assert_eq!(result.bindings, catalog.bindings_for(&roots));
                        prop_assert!(result.packages.is_empty());
                        coverage.reads += 1;
                    } else {
                        coverage.read_refusals += 1;
                    }
                } else {
                    coverage.absent_slots += 1;
                }
            }
        }
        for slot in 0..SLOTS {
            if let (Some(view), Some(observed)) = (&model[slot], &actual[slot]) {
                prop_assert_eq!(
                    observed.selected_native_groups(),
                    catalog.selection(&groups(&view.facts)),
                    "step {}, slot {}",
                    step,
                    slot
                );
                prop_assert_eq!(
                    observed
                        .descriptors()
                        .iter()
                        .map(|d| d.id)
                        .collect::<BTreeSet<_>>(),
                    carrier_ids(&view.facts),
                    "step {}, slot {}",
                    step,
                    slot
                );
            }
        }
        for inventory in 0..2 {
            let live = model
                .iter()
                .flatten()
                .filter(|view| view.inventory == inventory)
                .flat_map(|view| view.facts.iter().copied())
                .collect::<BTreeSet<_>>();
            if prior_live[inventory] > live.len() {
                coverage.reclaimed += prior_live[inventory] - live.len();
            }
            prior_live[inventory] = live.len();
            prop_assert_eq!(
                inventories[inventory].node_count(),
                live.len(),
                "step {}, inventory {}, {:?}",
                step,
                inventory,
                op
            );
        }
    }
    drop(actual);
    for inventory in &inventories {
        prop_assert_eq!(inventory.node_count(), 0, "all parents and clones released");
    }
    Ok(coverage)
}

fn property_config() -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 64;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

#[test]
fn staged_future_interface_does_not_widen_prior_views() {
    let catalog = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    let coverage = run_history(&catalog, &prefix()).unwrap();
    assert!(coverage.admissions >= 6 && coverage.refusals >= 2);
    assert!(coverage.reads >= 2 && coverage.read_refusals >= 3);
    assert!(coverage.repeated >= 1 && coverage.cross_merges >= 1 && coverage.reclaimed > 0);
    eprintln!("native staged regression coverage: {coverage:?}");
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn staged_native_histories_match_raw_fact_model(
        masks in proptest::collection::vec(0u8..16, MODULES),
        generations in proptest::collection::vec(0u64..8, 2 * MODULES),
        tail in operations(),
    ) {
        let catalog = Catalog::new(&masks, &generations, 1);
        let mut history = prefix(); history.extend(tail);
        let coverage = run_history(&catalog, &history)?;
        prop_assert!(coverage.admissions >= 6 && coverage.refusals >= 2);
        prop_assert!(coverage.clones > 0 && coverage.projections > 0 && coverage.merges >= 2);
        prop_assert!(coverage.cross_merges > 0 && coverage.reclaimed > 0 && coverage.read_refusals >= 3);
        eprintln!("native history coverage: {coverage:?}");
    }
}

#[test]
fn zero_edge_groups_change_view_equality_without_changing_carriers() {
    let catalog = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let carrier = inventory
        .admit_recovery_selection(&empty, catalog.entries(false, false), &BTreeSet::new())
        .unwrap();
    let selected = inventory
        .admit_recovery_selection(
            &carrier,
            Vec::new(),
            &BTreeSet::from([catalog.key((0, 29))]),
        )
        .unwrap();
    assert_eq!(carrier.descriptors(), selected.descriptors());
    assert_ne!(carrier, selected);
    assert!(carrier
        .native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: catalog.key((0, 29)).artifact,
            original_ordinal: 29
        }])
        .is_err());
    assert!(selected
        .native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: catalog.key((0, 29)).artifact,
            original_ordinal: 29
        }])
        .unwrap()
        .bindings
        .is_empty());
}

#[test]
fn whole_product_demand_requires_future_interface_atomically() {
    let catalog = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let early_keys = catalog.selection(&catalog.group_closure(&root_groups(0)));
    let early = inventory
        .admit_recovery_selection(&empty, catalog.entries(false, false), &early_keys)
        .unwrap();
    let before = inventory.node_count();
    assert!(inventory
        .admit_shared(&early, catalog.entries(false, true))
        .is_err());
    assert_eq!(inventory.node_count(), before);
    assert_eq!(early.selected_native_groups(), early_keys);
    let complete = inventory
        .admit_shared(&early, catalog.entries(true, true))
        .unwrap();
    assert_eq!(
        complete.selected_native_groups(),
        catalog.selection(&root_groups(3))
    );
    assert!(complete
        .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
            catalog.originals[0].descriptor.id
        )])
        .is_ok());
    assert!(early
        .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
            catalog.originals[0].descriptor.id
        )])
        .is_err());
    assert_eq!(early.selected_native_groups(), early_keys);
}

#[test]
fn exact_original_versions_do_not_substitute_across_views_or_reuse_indices() {
    let old = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    let new = Catalog::new(&[0; MODULES], &[2; 2 * MODULES], 2);
    assert_ne!(
        old.originals[1].descriptor.id,
        new.originals[1].descriptor.id
    );
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let old_groups = old.group_closure(&root_groups(0));
    let new_groups = new.group_closure(&root_groups(0));
    let old_view = inventory
        .admit_recovery_selection(
            &empty,
            old.entries(false, false),
            &old.selection(&old_groups),
        )
        .unwrap();
    let new_view = inventory
        .admit_recovery_selection(
            &empty,
            new.entries(false, true),
            &new.selection(&new_groups),
        )
        .unwrap();
    for (view, outside) in [(&old_view, new.key((1, 3))), (&new_view, old.key((1, 3)))] {
        assert!(view
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: outside.artifact,
                original_ordinal: outside.original_ordinal,
            }])
            .is_err());
    }
    let before = inventory.node_count();
    let merged = old_view.merge(&new_view).unwrap();
    assert_eq!(inventory.node_count(), before);
    let mut both = old.selection(&old_groups);
    both.extend(new.selection(&new_groups));
    assert_eq!(merged.selected_native_groups(), both);
    assert!(merged
        .metadata_snapshot()
        .validate_native_selection()
        .is_err());
    let projected = merged
        .select_roots(vec![old.originals[0].descriptor.id])
        .unwrap();
    assert_eq!(
        projected.selected_native_groups(),
        old.selection(&old_groups)
    );
    assert!(projected
        .metadata_snapshot()
        .validate_native_selection()
        .is_ok());
    drop(projected);
    drop(merged);
    assert_eq!(
        old_view
            .native_binding_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: old.key((0, 3)).artifact,
                original_ordinal: 3,
            }])
            .unwrap(),
        old.bindings_for(&root_groups(0))
    );
    assert_eq!(
        new_view
            .native_binding_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: new.key((0, 3)).artifact,
                original_ordinal: 3,
            }])
            .unwrap(),
        new.bindings_for(&root_groups(0))
    );
    let old_clone = old_view.clone();
    drop(old_view);
    assert_eq!(
        inventory.node_count(),
        before,
        "a clone owns the old version"
    );
    drop(old_clone);
    assert_eq!(
        inventory.node_count(),
        new.facts(new.supplied_ids(false), new_groups.clone()).len()
    );
    let replay = inventory
        .admit_recovery_selection(
            &empty,
            old.entries(false, true),
            &old.selection(&old_groups),
        )
        .unwrap();
    assert_eq!(replay.selected_native_groups(), old.selection(&old_groups));
    assert!(
        new_view
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: old.key((0, 3)).artifact,
                original_ordinal: 3,
            }])
            .is_err(),
        "reused graph storage cannot widen an older view"
    );
    drop(replay);
    drop(new_view);
    drop(empty);
    assert_eq!(inventory.node_count(), 0);
}

#[test]
fn selected_demand_keeps_authentication_of_needed_and_unused_original_bytes() {
    let catalog = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    let originals = catalog
        .originals
        .iter()
        .map(|entry| {
            let ArtifactPayload::Original(product) = &entry.payload else {
                unreachable!()
            };
            product
        })
        .collect::<Vec<_>>();
    let accepted = crate::certified_products::certify_owned_products_in_context_with_validation(
        &originals,
        &[],
        &originals,
        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(accepted.len(), MODULES * ORDINALS.len());
    let early = catalog.group_closure(&root_groups(0));
    assert!(early.contains(&(1, 3)) && !early.contains(&(0, 11)));
    // One mutation hits the demanded SCC helper, the other an unselected group
    // in the root's full product. Canonical attachment preserves the body; full
    // original certification authenticates it before selecting native demand.
    for (node, ordinal) in [(1, 3), (0, 11)] {
        let ArtifactPayload::Original(product) = &catalog.originals[node].payload else {
            unreachable!()
        };
        let mut bytes = product.product_bytes().to_vec();
        let marker = format!("entry_{ordinal}");
        let offset = bytes
            .windows(marker.len())
            .position(|window| window == marker.as_bytes())
            .unwrap();
        bytes[offset] ^= 1;
        let corrupt = CertifiedRecoveryProduct::from_certification(
            product.owner().clone(),
            product.interface_bytes().to_vec(),
            bytes,
            product.package_imports_bytes().to_vec(),
            product.certification_bytes().to_vec(),
        );
        let corrupt = corrupt
            .with_module_interface(product.module_interface().unwrap().clone())
            .unwrap();
        let mut changed = originals.clone();
        changed[node] = &corrupt;
        assert!(matches!(
            crate::certified_products::certify_owned_products_in_context_with_validation(
                &changed,
                &[],
                &changed,
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            ),
            Err(crate::certified_products::CertificationError::Mismatch(
                "inherited artifact bytes"
            ))
        ));
    }
    let inventory = ArtifactInventory::default();
    let mut entries = catalog.originals.clone();
    // Unlike a future interface, the baseline interface is needed by the
    // selected closure, so refusal precedes every inventory mutation.
    let before = inventory.node_count();
    assert!(inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            entries.clone(),
            &catalog.selection(&catalog.group_closure(&root_groups(0)))
        )
        .is_err());
    assert_eq!(inventory.node_count(), before);
    entries.push(catalog.values[0].clone());
    assert!(inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            entries,
            &catalog.selection(&catalog.group_closure(&root_groups(0)))
        )
        .is_ok());
}

fn check_unrecorded_global_dependency(
    catalog: &Catalog,
    reverse: bool,
) -> Result<(), TestCaseError> {
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let carrier = inventory
        .admit_recovery_selection(&empty, catalog.entries(true, reverse), &BTreeSet::new())
        .unwrap();
    let early_groups = catalog.group_closure(&root_groups(0));
    let global_early = inventory
        .admit_recovery_selection(&carrier, Vec::new(), &catalog.selection(&early_groups))
        .unwrap();
    let late_groups = catalog.group_closure(&root_groups(1));
    prop_assert!(late_groups.contains(&(1, 3)) && early_groups.contains(&(1, 3)));
    let mut missing = late_groups.clone();
    missing.remove(&(1, 3));
    let before = inventory.node_count();
    // The dependency exists in this inventory, but neither the parent view nor
    // the replayed closed selection authorizes it. New-node-only validation
    // would accept this history and silently recover a wider selection.
    let replay =
        inventory.admit_recovery_selection(&carrier, Vec::new(), &catalog.selection(&missing));
    prop_assert!(
        replay.is_err(),
        "saved selection omitted a globally registered dependency"
    );
    prop_assert_eq!(inventory.node_count(), before);
    prop_assert!(carrier.selected_native_groups().is_empty());
    prop_assert_eq!(
        global_early.selected_native_groups(),
        catalog.selection(&early_groups)
    );
    let valid = inventory
        .admit_recovery_selection(&carrier, Vec::new(), &catalog.selection(&late_groups))
        .unwrap();
    prop_assert_eq!(
        valid.selected_native_groups(),
        catalog.selection(&late_groups)
    );
    Ok(())
}

#[test]
fn recovery_selection_cannot_borrow_unrecorded_global_dependency() {
    let catalog = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    for reverse in [false, true] {
        check_unrecorded_global_dependency(&catalog, reverse).unwrap();
    }
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn omitted_replay_edge_refuses_even_when_dependency_is_already_registered(
        masks in proptest::collection::vec(0u8..16, MODULES),
        generations in proptest::collection::vec(0u64..8, 2 * MODULES),
        reverse in any::<bool>(),
    ) {
        let catalog = Catalog::new(&masks, &generations, 1);
        check_unrecorded_global_dependency(&catalog, reverse)?;
    }
}
