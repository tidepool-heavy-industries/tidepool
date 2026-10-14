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

// Scope IDs are opaque issuer outputs. Only their equality is observed; rows and
// reachability are derived from the catalog. This does not independently test
// the issuer's digest/allocation law.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Physical {
    scope: usize,
    fact: Fact,
}

#[derive(Default)]
struct Rows {
    scopes: BTreeMap<ArtifactSelectionId, usize>,
    handles: BTreeMap<Physical, ArtifactBindingNode>,
    edges: BTreeMap<Physical, BTreeSet<(Physical, ArtifactDependency)>>,
}

impl Rows {
    fn register(&mut self, catalog: &Catalog, handle: ArtifactBindingNode) -> Physical {
        let (binding, fact) = match handle {
            ArtifactBindingNode::Artifact(binding) => (binding, Fact::Carrier(binding.artifact)),
            ArtifactBindingNode::Group(group) => {
                let node = catalog
                    .originals
                    .iter()
                    .position(|entry| entry.descriptor.id == group.binding.artifact)
                    .expect("issued group must name a catalog original");
                assert!(
                    ORDINALS.contains(&group.original_ordinal),
                    "unexpected issued ordinal"
                );
                (group.binding, Fact::Group((node, group.original_ordinal)))
            }
        };
        let next = self.scopes.len();
        let scope = *self.scopes.entry(binding.selection).or_insert(next);
        let key = Physical { scope, fact };
        // No alias may overwrite an earlier correspondence, including one whose
        // live inventory rows have since been released.
        if let Some(prior) = self.handles.get(&key) {
            assert_eq!(*prior, handle, "issuer handle correspondence is immutable");
        } else {
            assert!(!self.handles.values().any(|prior| *prior == handle));
            self.handles.insert(key, handle);
        }
        key
    }

    fn close(&self, roots: BTreeSet<Physical>) -> BTreeSet<Physical> {
        let mut closed = roots;
        loop {
            let prior = closed.clone();
            for row in &prior {
                closed.extend(self.edges[row].iter().map(|(target, _)| *target));
            }
            if closed == prior {
                return closed;
            }
        }
    }

    fn carrier(&self, group: Physical) -> Physical {
        let carriers = self.edges[&group]
            .iter()
            .filter_map(|(target, edge)| {
                matches!(edge, ArtifactDependency::Interface).then_some(*target)
            })
            .collect::<Vec<_>>();
        assert_eq!(carriers.len(), 1, "exactly one group carrier");
        carriers[0]
    }

    fn resolve(&self, view: &View, id: ArtifactId) -> Option<Physical> {
        if let Some(carrier) = view.namespace.get(&id) {
            return Some(*carrier);
        }
        let carriers = view
            .physical
            .iter()
            .filter(|row| row.fact == Fact::Carrier(id))
            .copied()
            .collect::<Vec<_>>();
        (carriers.len() == 1).then(|| carriers[0])
    }

    fn group(&self, view: &View, raw: RawGroup, carrier: Physical) -> Option<Physical> {
        let matches = view
            .physical
            .iter()
            .filter(|row| row.fact == Fact::Group(raw) && self.carrier(**row) == carrier)
            .copied()
            .collect::<Vec<_>>();
        (matches.len() == 1).then(|| matches[0])
    }

    fn compatible(&self, group: Physical, namespace: &BTreeMap<ArtifactId, Physical>) -> bool {
        // Reuse means the entire independently expanded carrier closure fits
        // the current issuer's choices, including retained-value interfaces.
        self.close(BTreeSet::from([group]))
            .iter()
            .all(|row| match row.fact {
                Fact::Carrier(id) => namespace.get(&id) == Some(row),
                Fact::Group(_) => true,
            })
    }

    fn admit(
        &mut self,
        catalog: &Catalog,
        parent: &View,
        issued: &ArtifactView,
        available: &BTreeSet<ArtifactId>,
        demanded: &BTreeSet<RawGroup>,
    ) -> View {
        let mut planned_groups = demanded.clone();
        planned_groups.extend(groups(&parent.facts()));
        let graph = issued.capture_graph_selection();
        // These are identity observations at the issuance boundary, never the
        // source of expected edges or expected reachable membership.
        assert_eq!(
            graph
                .namespace
                .iter()
                .map(|binding| binding.artifact)
                .collect::<BTreeSet<_>>(),
            *available
        );
        assert_eq!(
            graph.namespace.len(),
            available.len(),
            "namespace identities are unique"
        );
        assert_eq!(
            graph
                .bindings
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            graph.bindings.len()
        );
        assert_eq!(
            graph
                .native_groups
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            graph.native_groups.len()
        );
        let namespace = graph
            .namespace
            .iter()
            .map(|binding| {
                (
                    binding.artifact,
                    self.register(catalog, ArtifactBindingNode::Artifact(*binding)),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let issued_groups = graph
            .native_groups
            .iter()
            .map(|group| self.register(catalog, ArtifactBindingNode::Group(*group)))
            .collect::<BTreeSet<_>>();
        for row in namespace.values() {
            if !self.edges.contains_key(row) {
                let mut edges = BTreeSet::new();
                if let Fact::Carrier(id) = row.fact {
                    if let Some(node) = catalog
                        .originals
                        .iter()
                        .position(|entry| entry.descriptor.id == id)
                    {
                        edges.insert((
                            namespace[&catalog.interfaces[node].descriptor.id],
                            ArtifactDependency::Interface,
                        ));
                    }
                }
                self.edges.insert(*row, edges);
            }
        }
        for row in &issued_groups {
            if self.edges.contains_key(row) {
                continue;
            }
            let Fact::Group(raw) = row.fact else {
                unreachable!()
            };
            assert!(
                planned_groups.contains(&raw),
                "new group outside requested and inherited raw closure"
            );
            let carrier = namespace[&catalog.originals[raw.0].descriptor.id];
            let mut edges = BTreeSet::from([(carrier, ArtifactDependency::Interface)]);
            for target in catalog.edges.get(&raw).into_iter().flatten() {
                let same_issue = Physical {
                    scope: row.scope,
                    fact: Fact::Group(*target),
                };
                let dependency = if issued_groups.contains(&same_issue) {
                    same_issue
                } else {
                    let chosen = namespace[&catalog.originals[target.0].descriptor.id];
                    let candidates = issued_groups
                        .iter()
                        .filter(|group| {
                            group.fact == Fact::Group(*target)
                                && self.edges.contains_key(group)
                                && self.carrier(**group) == chosen
                                && self.compatible(**group, &namespace)
                        })
                        .copied()
                        .collect::<Vec<_>>();
                    assert_eq!(
                        candidates.len(),
                        1,
                        "issued dependency needs one exact prior group"
                    );
                    candidates[0]
                };
                edges.insert((
                    dependency,
                    ArtifactDependency::NativeGroup {
                        dependent_ordinal: raw.1,
                        required_ordinal: target.1,
                    },
                ));
            }
            if let Some((value, generation)) = catalog.bindings.get(&raw) {
                let identity = binding_identity(*value, raw);
                edges.insert((
                    namespace[&catalog.values[*value].descriptor.id],
                    ArtifactDependency::NativeBinding {
                        dependent_ordinal: raw.1,
                        generation: *generation,
                        namespace: identity.namespace,
                        occurrence: identity.occurrence,
                        record_parent: identity.record_parent,
                    },
                ));
            }
            self.edges.insert(*row, edges);
        }
        let mut selected = namespace.values().copied().collect::<BTreeSet<_>>();
        let offer = View {
            inventory: parent.inventory,
            physical: issued_groups.clone(),
            namespace: namespace.clone(),
            roots: BTreeSet::new(),
            demand: BTreeSet::new(),
        };
        for raw in &planned_groups {
            let carrier = namespace[&catalog.originals[raw.0].descriptor.id];
            let fresh = issued_groups
                .iter()
                .filter(|row| row.fact == Fact::Group(*raw) && !parent.physical.contains(row))
                .copied()
                .collect::<Vec<_>>();
            assert!(
                fresh.len() <= 1,
                "one newly issued identity per logical group"
            );
            if let Some(chosen) = fresh.first() {
                assert_eq!(
                    self.carrier(*chosen),
                    carrier,
                    "newly planned group uses chosen carrier"
                );
                selected.insert(*chosen);
            } else {
                // An issuer can return an already-held exact row when two
                // branches issued equal group rows under the same carrier.
                // Both remain custody; ordinary admission does not make later
                // carrier-qualified reads unambiguous.
                let matching = offer
                    .physical
                    .iter()
                    .filter(|row| {
                        row.fact == Fact::Group(*raw)
                            && self.carrier(**row) == carrier
                            && self.compatible(**row, &namespace)
                    })
                    .copied()
                    .collect::<BTreeSet<_>>();
                assert!(
                    !matching.is_empty(),
                    "planned group must have a qualified issuing identity"
                );
                assert!(
                    matching.is_subset(&parent.physical),
                    "unobserved fresh issuing identity"
                );
                selected.extend(matching);
            }
        }

        let mut physical = self.close(selected);
        physical.extend(&parent.physical);
        View {
            inventory: parent.inventory,
            physical,
            namespace,
            roots: parent.roots.clone(),
            demand: catalog.group_closure(&parent.demand.union(demanded).copied().collect()),
        }
    }
}

#[derive(Clone, Debug)]
struct View {
    inventory: usize,
    physical: BTreeSet<Physical>,
    namespace: BTreeMap<ArtifactId, Physical>,
    // Artifact-root observation and closed executable demand are distinct
    // from the complete physical byte custody.
    roots: BTreeSet<ArtifactId>,
    demand: BTreeSet<RawGroup>,
}

impl View {
    fn facts(&self) -> BTreeSet<Fact> {
        self.physical.iter().map(|row| row.fact).collect()
    }
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
    Recover {
        from: usize,
        inventory: usize,
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
        5 => BTreeSet::from([(3, 29)]),
        _ => BTreeSet::new(),
    }
}

fn operations() -> impl Strategy<Value = Vec<Op>> {
    let slot = 0..SLOTS;
    proptest::collection::vec(
        prop_oneof![
            1 => (0usize..2, slot.clone()).prop_map(|(inventory, to)| Op::Empty { inventory, to }),
            5 => (slot.clone(), 0u8..6, any::<bool>(), any::<bool>(), slot.clone())
                .prop_map(|(parent, root, future, reverse, to)| Op::Admit { parent, root, future, reverse, to }),
            2 => (slot.clone(), slot.clone()).prop_map(|(from, to)| Op::Clone { from, to }),
            2 => slot.clone().prop_map(Op::Drop),
            3 => (slot.clone(), proptest::collection::vec(0..MODULES, 0..5), slot.clone())
                .prop_map(|(from, nodes, to)| Op::Project { from, nodes, to }),
            3 => (slot.clone(), slot.clone(), slot.clone()).prop_map(|(left, right, to)| Op::Merge { left, right, to }),
            2 => (slot.clone(), 0usize..2, slot.clone()).prop_map(|(from, inventory, to)| Op::Recover { from, inventory, to }),
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
    recoveries: usize,
    namespace_masks: usize,
    carrier_qualified_refusals: usize,
    duplicate_scope_views: usize,
    cross_scope_group_carriers: usize,
    indexed_unique: usize,
    indexed_unavailable: usize,
    indexed_ambiguous: usize,
    custody_only_views: usize,
    custody_only_reads: usize,
    selected_retained_body: usize,
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
        Op::Read {
            from: 4,
            node: 0,
            ordinal: 3,
            all: false,
        },
        Op::Read {
            from: 4,
            node: 0,
            ordinal: 29,
            all: false,
        },
        Op::Recover {
            from: 4,
            inventory: 1,
            to: 5,
        },
        Op::Project {
            from: 5,
            nodes: vec![0],
            to: 5,
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
        Op::Project {
            from: 2,
            nodes: vec![0],
            to: 3,
        },
        Op::Project {
            from: 5,
            nodes: vec![0],
            to: 5,
        },
        Op::Merge {
            left: 3,
            right: 5,
            to: 7,
        },
    ]
}

// The oracle intentionally retains the old linear iterator and reads graph
// edges directly, independently of the index's construction and lookup.
fn check_native_selection_index(
    view: &ArtifactView,
    coverage: &mut Coverage,
) -> Result<(), TestCaseError> {
    fn outcome(
        result: Result<NativeGroupBinding, CompileError>,
    ) -> Result<NativeGroupBinding, ArtifactInventoryFailure> {
        result.map_err(|error| match error {
            CompileError::ArtifactInventory(error) => error.failure,
            other => panic!("unexpected group lookup failure: {other:?}"),
        })
    }
    let state = view.lease.inventory.0.lock().expect("inventory lock");
    let projection = view.read_projection(&state);
    let index = projection.native_selection(&state);
    prop_assert!(std::ptr::eq(
        index,
        view.clone()
            .read_projection(&state)
            .native_selection(&state)
    ));
    let mut ordinals = projection
        .nodes
        .iter()
        .filter_map(|node| match node {
            InventoryNodeKey::Group(group) => Some(group.original_ordinal),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    ordinals.insert(u32::MAX);
    for node in &projection.nodes {
        let InventoryNodeKey::Artifact(carrier) = node else {
            continue;
        };
        let expected_ordinals = projection
            .nodes
            .iter()
            .filter_map(|node| match node {
                InventoryNodeKey::Group(group) if group.binding.artifact == carrier.artifact => {
                    Some(group.original_ordinal)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        prop_assert_eq!(
            index
                .ordinals
                .get(&carrier.artifact)
                .cloned()
                .unwrap_or_default(),
            expected_ordinals
        );
        for &ordinal in &ordinals {
            let actual = outcome(index.resolve(*carrier, ordinal));
            let expected = outcome(resolve_group_binding(
                &state,
                &projection.nodes,
                *carrier,
                ordinal,
            ));
            prop_assert_eq!(&actual, &expected);
            match actual {
                Ok(_) => coverage.indexed_unique += 1,
                Err(ArtifactInventoryFailure::NativeGroupUnavailable { .. }) => {
                    coverage.indexed_unavailable += 1
                }
                Err(ArtifactInventoryFailure::BindingAmbiguity { .. }) => {
                    coverage.indexed_ambiguous += 1
                }
                Err(other) => panic!("unexpected index outcome: {other:?}"),
            }
        }
    }
    Ok(())
}

fn run_history(catalog: &Catalog, ops: &[Op]) -> Result<Coverage, TestCaseError> {
    let mut coverage = Coverage::default();
    let result = run_history_observed(catalog, ops, &mut coverage);
    if result.is_err() {
        eprintln!("native failed history coverage: {coverage:?}; operations={ops:?}");
    }
    result?;
    Ok(coverage)
}

fn run_history_observed(
    catalog: &Catalog,
    ops: &[Op],
    coverage: &mut Coverage,
) -> Result<(), TestCaseError> {
    let inventories = [ArtifactInventory::default(), ArtifactInventory::default()];
    let mut actual: [Option<ArtifactView>; SLOTS] = std::array::from_fn(|_| None);
    let mut model: [Option<View>; SLOTS] = std::array::from_fn(|_| None);
    let mut rows = Rows::default();
    let mut prior_live = [0; 2];
    for (step, op) in ops.iter().enumerate() {
        match op {
            Op::Empty { inventory, to } => {
                actual[*to] = Some(inventories[*inventory].empty_view());
                model[*to] = Some(View {
                    inventory: *inventory,
                    physical: BTreeSet::new(),
                    namespace: BTreeMap::new(),
                    roots: BTreeSet::new(),
                    demand: BTreeSet::new(),
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
                    let supplied = catalog.supplied_ids(*future);
                    let mut available = carrier_ids(&view.facts());
                    available.extend(&supplied);
                    let demanded = catalog.group_closure(&root_groups(*root));
                    let wanted = catalog.facts(available.clone(), demanded.clone());
                    let expected = carrier_ids(&wanted).is_subset(&available);
                    let before = inventories[view.inventory].node_count();
                    let result = inventories[view.inventory].admit_recovery_selection(
                        actual[*parent].as_ref().unwrap(),
                        catalog.entries(*future, *reverse),
                        &catalog.selection(&demanded),
                    );
                    prop_assert_eq!(
                        result.is_ok(),
                        expected,
                        "step {}: {:?}; {:?}",
                        step,
                        op,
                        result
                    );
                    if let Ok(result) = result {
                        let mut admitted =
                            rows.admit(catalog, &view, &result, &available, &demanded);
                        admitted.roots = view.roots.clone();
                        admitted.roots.extend(supplied);
                        admitted
                            .roots
                            .extend(demanded.iter().map(|raw| catalog.key(*raw).artifact));
                        if admitted.physical == view.physical {
                            coverage.repeated += 1;
                        }
                        actual[*to] = Some(result);
                        model[*to] = Some(admitted);
                        coverage.selected_retained_body += demanded
                            .intersection(&groups(&view.facts()))
                            .filter(|group| !view.demand.contains(group))
                            .count();
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
                    let carriers = ids
                        .iter()
                        .map(|id| rows.resolve(&view, *id))
                        .collect::<Option<BTreeSet<_>>>();
                    let result = actual[*from]
                        .as_ref()
                        .unwrap()
                        .select_roots(ids.iter().copied().collect());
                    let unambiguous = carriers.as_ref().is_some_and(|carriers| carriers.iter().all(|carrier|
                        ORDINALS.iter().all(|ordinal| view.physical.iter().filter(|row| matches!(row.fact,
                            Fact::Group((node, selected)) if catalog.originals[node].descriptor.id == match carrier.fact { Fact::Carrier(id) => id, _ => unreachable!() } && selected == *ordinal)
                            && rows.carrier(**row) == *carrier).count() <= 1)));
                    prop_assert_eq!(result.is_ok(), unambiguous, "step {}: {:?}", step, op);
                    if let Ok(result) = result {
                        let mut selected = carriers.unwrap();
                        for node in nodes {
                            let carrier = rows
                                .resolve(&view, catalog.originals[*node].descriptor.id)
                                .unwrap();
                            for ordinal in ORDINALS {
                                if let Some(group) = rows.group(&view, (*node, ordinal), carrier) {
                                    selected.insert(group);
                                }
                            }
                        }
                        let selected_closure = rows.close(selected);
                        let demand = groups(&selected_closure.iter().map(|row| row.fact).collect());
                        let mut physical = selected_closure.clone();
                        // A retained canonical interface can keep several exact
                        // original carriers. Their recorded canonical edge must
                        // land in the independently selected interface closure.
                        for group in view
                            .physical
                            .iter()
                            .filter(|row| matches!(row.fact, Fact::Group(_)))
                        {
                            let carrier = rows.carrier(*group);
                            if rows.edges[&carrier].iter().any(|(target, edge)| {
                                *edge == ArtifactDependency::Interface
                                    && selected_closure.contains(target)
                            }) {
                                physical.insert(carrier);
                            }
                        }
                        // All bodies' source bytes retain available dependency
                        // carriers; this adds no executable group demand.
                        loop {
                            let prior = physical.clone();
                            for row in &prior {
                                if let Fact::Carrier(id) = row.fact {
                                    if let Some(node) = catalog
                                        .originals
                                        .iter()
                                        .position(|entry| entry.descriptor.id == id)
                                    {
                                        for ((source, _), targets) in &catalog.edges {
                                            if *source == node {
                                                for (target, _) in targets {
                                                    if let Some(carrier) = rows.resolve(
                                                        &view,
                                                        catalog.originals[*target].descriptor.id,
                                                    ) {
                                                        physical.insert(carrier);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // An already issued body belongs to retained byte
                            // custody whenever its exact carrier remains held.
                            // This does not add it to the demand closure above.
                            physical.extend(
                                view.physical
                                    .iter()
                                    .filter(|row| {
                                        matches!(row.fact, Fact::Group(_))
                                            && physical.contains(&rows.carrier(**row))
                                    })
                                    .copied()
                                    .collect::<Vec<_>>(),
                            );
                            physical = rows.close(physical);
                            if physical == prior {
                                break;
                            }
                        }
                        let namespace = view
                            .namespace
                            .iter()
                            .filter(|(_, carrier)| physical.contains(carrier))
                            .map(|(id, carrier)| (*id, *carrier))
                            .collect();
                        actual[*to] = Some(result);
                        model[*to] = Some(View {
                            inventory: view.inventory,
                            physical,
                            namespace,
                            roots: ids,
                            demand,
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
                    let inventory = if right_view.physical.is_empty() {
                        left_view.inventory
                    } else if left_view.physical.is_empty() {
                        right_view.inventory
                    } else {
                        left_view.inventory
                    };
                    let mut physical = left_view.physical.clone();
                    physical.extend(&right_view.physical);
                    coverage.namespace_masks += right_view
                        .namespace
                        .iter()
                        .filter(|(id, carrier)| {
                            left_view
                                .namespace
                                .get(id)
                                .is_some_and(|old| old != *carrier)
                        })
                        .count();
                    let mut namespace = left_view.namespace;
                    namespace.extend(right_view.namespace);
                    let mut roots = left_view.roots;
                    roots.extend(right_view.roots);
                    let mut demand = left_view.demand;
                    demand.extend(right_view.demand);
                    actual[*to] = Some(result.unwrap());
                    model[*to] = Some(View {
                        inventory,
                        physical,
                        namespace,
                        roots,
                        demand,
                    });
                    coverage.merges += 1;
                    if left_view.inventory != right_view.inventory {
                        coverage.cross_merges += 1;
                    }
                } else {
                    coverage.absent_slots += 1;
                }
            }
            Op::Recover {
                from,
                inventory,
                to,
            } => {
                if let Some(mut view) = model[*from].clone() {
                    let original = actual[*from].as_ref().unwrap();
                    let result = inventories[*inventory].recover_selection(
                        &inventories[*inventory].empty_view(),
                        original.entries(),
                        &original.capture_graph_selection(),
                        &original.binding_dependencies(),
                    );
                    prop_assert!(
                        result.is_ok(),
                        "exact recovery at step {}: {:?}",
                        step,
                        result
                    );
                    view.inventory = *inventory;
                    actual[*to] = Some(result.unwrap());
                    model[*to] = Some(view);
                    coverage.recoveries += 1;
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
                    let requested = if *all {
                        ORDINALS
                            .map(|ordinal| (*node, ordinal))
                            .into_iter()
                            .collect::<BTreeSet<_>>()
                    } else {
                        BTreeSet::from([(*node, *ordinal)])
                    };
                    let selected = rows
                        .resolve(view, catalog.originals[*node].descriptor.id)
                        .and_then(|carrier| {
                            requested
                                .iter()
                                .map(|raw| rows.group(view, *raw, carrier))
                                .collect::<Option<BTreeSet<_>>>()
                        });
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
                        .native_requirements_with_groups_from_roots(&[root]);
                    prop_assert_eq!(
                        result.is_ok(),
                        selected.is_some(),
                        "step {}: {:?}",
                        step,
                        op
                    );
                    if let Ok((result, actual_selected)) = result {
                        let closure = rows.close(selected.unwrap());
                        let raw = closure
                            .iter()
                            .filter_map(|row| {
                                if let Fact::Group(group) = row.fact {
                                    Some(group)
                                } else {
                                    None
                                }
                            })
                            .collect();
                        prop_assert_eq!(
                            actual_selected,
                            catalog.selection(&raw),
                            "exact qualified closure at step {}",
                            step
                        );
                        prop_assert_eq!(result.bindings, catalog.bindings_for(&requested));
                        prop_assert!(result.packages.is_empty());
                        coverage.custody_only_reads +=
                            usize::from(!requested.is_subset(&view.demand));
                        coverage.reads += 1;
                    } else {
                        coverage.read_refusals += 1;
                        if requested.is_subset(&groups(&view.facts())) {
                            coverage.carrier_qualified_refusals += 1;
                        }
                    }
                } else {
                    coverage.absent_slots += 1;
                }
            }
        }
        for slot in 0..SLOTS {
            if let (Some(view), Some(observed)) = (&model[slot], &actual[slot]) {
                if view.physical.len() > view.facts().len() {
                    coverage.duplicate_scope_views += 1;
                }
                coverage.cross_scope_group_carriers += view
                    .physical
                    .iter()
                    .filter(|row| {
                        matches!(row.fact, Fact::Group(_)) && rows.carrier(**row).scope != row.scope
                    })
                    .count();
                coverage.custody_only_views +=
                    usize::from(!groups(&view.facts()).is_subset(&view.demand));
                check_native_selection_index(observed, coverage)?;
                let graph = observed.capture_graph_selection();
                let expected_handles = view
                    .physical
                    .iter()
                    .map(|row| rows.handles[row])
                    .collect::<BTreeSet<_>>();
                let actual_handles = graph
                    .bindings
                    .into_iter()
                    .map(ArtifactBindingNode::Artifact)
                    .chain(
                        graph
                            .native_groups
                            .into_iter()
                            .map(ArtifactBindingNode::Group),
                    )
                    .collect::<BTreeSet<_>>();
                prop_assert_eq!(
                    actual_handles,
                    expected_handles,
                    "physical closure at step {}, slot {}",
                    step,
                    slot
                );
                let expected_namespace = view
                    .namespace
                    .values()
                    .map(|row| match rows.handles[row] {
                        ArtifactBindingNode::Artifact(binding) => binding,
                        _ => unreachable!(),
                    })
                    .collect::<BTreeSet<_>>();
                prop_assert_eq!(
                    graph.namespace.into_iter().collect::<BTreeSet<_>>(),
                    expected_namespace,
                    "chosen namespace at step {}, slot {}",
                    step,
                    slot
                );
                let expected_edges = view
                    .physical
                    .iter()
                    .flat_map(|source| {
                        rows.edges[source].iter().map(|(target, edge)| {
                            (rows.handles[source], rows.handles[target], edge.clone())
                        })
                    })
                    .collect::<BTreeSet<_>>();
                prop_assert_eq!(
                    observed
                        .binding_dependencies()
                        .into_iter()
                        .collect::<BTreeSet<_>>(),
                    expected_edges,
                    "independent raw rows at step {}, slot {}",
                    step,
                    slot
                );
                prop_assert_eq!(
                    observed.selected_native_groups(),
                    catalog.selection(&view.demand),
                    "step {}, slot {}",
                    step,
                    slot
                );
                prop_assert_eq!(
                    observed
                        .root_entries()
                        .iter()
                        .map(|entry| entry.descriptor.id)
                        .collect::<BTreeSet<_>>(),
                    view.roots.clone(),
                    "root intent at step {}, slot {}",
                    step,
                    slot
                );
                prop_assert_eq!(
                    observed
                        .descriptors()
                        .iter()
                        .map(|d| d.id)
                        .collect::<BTreeSet<_>>(),
                    carrier_ids(&view.facts()),
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
                .flat_map(|view| view.physical.iter().copied())
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
    Ok(())
}

fn retained_body_history() -> Vec<Op> {
    vec![
        Op::Empty {
            inventory: 0,
            to: 0,
        },
        Op::Admit {
            parent: 0,
            root: 3,
            future: true,
            reverse: false,
            to: 1,
        },
        Op::Project {
            from: 1,
            nodes: vec![1],
            to: 2,
        },
        Op::Read {
            from: 2,
            node: 3,
            ordinal: 29,
            all: false,
        },
        Op::Drop(1),
        Op::Recover {
            from: 2,
            inventory: 1,
            to: 3,
        },
        Op::Read {
            from: 3,
            node: 3,
            ordinal: 29,
            all: false,
        },
        Op::Admit {
            parent: 3,
            root: 5,
            future: true,
            reverse: false,
            to: 4,
        },
        Op::Drop(2),
        Op::Drop(3),
        Op::Read {
            from: 4,
            node: 3,
            ordinal: 29,
            all: false,
        },
    ]
}

#[test]
fn projected_complete_carrier_custody_matches_minimized_history() {
    let catalog = Catalog::new(&[0, 0, 0, 1], &[0; 2 * MODULES], 1);
    let mut history = prefix();
    // The causal tail from the retained fail-before trace; its six trailing
    // empty operations were only the arbitrary strategy's minimum vector size.
    history.extend([
        Op::Admit {
            parent: 0,
            root: 1,
            future: true,
            reverse: false,
            to: 1,
        },
        Op::Project {
            from: 1,
            nodes: vec![1],
            to: 0,
        },
    ]);
    let coverage = run_history(&catalog, &history).unwrap();
    assert!(coverage.custody_only_views > 0, "{coverage:?}");
    eprintln!("complete carrier custody regression: {coverage:?}");
}

#[test]
fn retained_body_enters_demand_only_after_explicit_selection() {
    let catalog = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    let coverage = run_history(&catalog, &retained_body_history()).unwrap();
    assert!(coverage.custody_only_views > 0, "{coverage:?}");
    assert!(coverage.custody_only_reads >= 2, "{coverage:?}");
    assert!(coverage.selected_retained_body > 0, "{coverage:?}");

    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let all_groups = root_groups(3);
    let parent = inventory
        .admit_recovery_selection(
            &empty,
            catalog.entries(true, false),
            &catalog.selection(&all_groups),
        )
        .unwrap();
    let projected = parent
        .select_roots(vec![catalog.originals[1].descriptor.id])
        .unwrap();
    let body = catalog.key((3, 29));
    let graph = projected.capture_graph_selection();
    let held = *graph
        .native_groups
        .iter()
        .find(|group| {
            group.binding.artifact == body.artifact
                && group.original_ordinal == body.original_ordinal
        })
        .expect("the exact carrier keeps its already issued body bytes");
    let expected =
        catalog.group_closure(&ORDINALS.into_iter().map(|ordinal| (1, ordinal)).collect());
    assert_eq!(
        projected.selected_native_groups(),
        catalog.selection(&expected)
    );
    assert!(!projected.selected_native_groups().contains(&body));
    let available = projected
        .native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: body.artifact,
            original_ordinal: body.original_ordinal,
        }])
        .unwrap();
    assert!(available.bindings.is_empty() && available.packages.is_empty());
    assert_eq!(
        projected.selected_native_groups(),
        catalog.selection(&expected),
        "a query does not select demand"
    );
    drop(parent);
    let mut requested = expected.clone();
    requested.insert((3, 29));
    let selected = inventory
        .admit_recovery_selection(&projected, Vec::new(), &catalog.selection(&requested))
        .unwrap();
    assert_eq!(
        selected.selected_native_groups(),
        catalog.selection(&requested)
    );
    assert!(
        selected
            .capture_graph_selection()
            .native_groups
            .contains(&held),
        "selection reuses the exact retained issuing body"
    );
    assert_eq!(
        projected.selected_native_groups(),
        catalog.selection(&expected),
        "selection never widens the older view"
    );
    drop(projected);
    drop(selected);
    drop(empty);
    assert_eq!(inventory.node_count(), 0);
}

#[derive(Default, Debug)]
struct ColdNativeCoverage {
    callbacks: usize,
    completed: usize,
    eager_reads: usize,
    delayed_reads: usize,
    slot_reuses: usize,
    recoveries: usize,
    cross_scope_carriers: usize,
}

fn check_cold_native_projection(
    view: &ArtifactView,
    catalog: &Catalog,
    selected: &BTreeSet<RawGroup>,
    roots: Option<&[usize]>,
) -> Result<(), TestCaseError> {
    // The initial diamond reaches every supplied original. Expected payloads,
    // native selection and binding requirements come from authored fixture facts.
    prop_assert_eq!(
        view.artifact_ids().into_iter().collect::<BTreeSet<_>>(),
        carrier_ids(&catalog.facts(catalog.supplied_ids(false), selected.clone()))
    );
    prop_assert_eq!(view.selected_native_groups(), catalog.selection(selected));
    prop_assert_eq!(
        view.root_entries()
            .into_iter()
            .map(|entry| entry.descriptor.id)
            .collect::<BTreeSet<_>>(),
        roots.map_or_else(
            || catalog.supplied_ids(false),
            |roots| roots
                .iter()
                .map(|node| catalog.originals[*node].descriptor.id)
                .collect::<BTreeSet<_>>()
        )
    );
    let (requirements, groups) = view
        .native_requirements_with_groups_from_roots(&[NativeRequirementRoot::Group {
            artifact: catalog.key((0, 3)).artifact,
            original_ordinal: 3,
        }])
        .unwrap();
    prop_assert_eq!(requirements.bindings, catalog.bindings_for(&root_groups(0)));
    prop_assert!(requirements.packages.is_empty());
    prop_assert_eq!(
        groups,
        catalog.selection(&catalog.group_closure(&root_groups(0)))
    );
    prop_assert!(
        view.native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: catalog.key((0, 11)).artifact,
            original_ordinal: 11,
        },])
            .is_err(),
        "available original bytes do not select the future body"
    );
    Ok(())
}

fn cold_native_history(
    catalog: &Catalog,
    schedule: [bool; 3],
    coverage: &mut ColdNativeCoverage,
) -> Result<(), TestCaseError> {
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let selected = catalog.group_closure(&root_groups(0));
    let retired = Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            [9; 32],
            "fixture",
            "ColdRetired",
            BTreeMap::new(),
        ),
    ));
    let mut entries = catalog.entries(false, false);
    entries.push(retired.clone());
    let parent = inventory
        .admit_recovery_selection(&empty, entries, &catalog.selection(&selected))
        .unwrap();
    let projection = parent
        .select_roots(vec![catalog.originals[0].descriptor.id])
        .unwrap();
    let alias = projection.clone();
    let left = parent
        .select_roots(vec![catalog.originals[0].descriptor.id])
        .unwrap();
    let right = parent
        .select_roots(vec![catalog.originals[1].descriptor.id])
        .unwrap();
    let merged = left.merge(&right).unwrap();
    let parent_reads = Arc::downgrade(&parent.reads);
    let left_reads = Arc::downgrade(&left.reads);
    let right_reads = Arc::downgrade(&right.reads);
    prop_assert!(Arc::ptr_eq(&projection.lease, &alias.lease));
    prop_assert!(Arc::ptr_eq(&projection.reads, &alias.reads));
    // Only explicit operations may initialize these outputs. The checker never
    // uses public getters merely to establish that an output is still cold.
    prop_assert!(projection.reads.read_projection.get().is_none());
    prop_assert!(merged.reads.read_projection.get().is_none());
    for (index, (view, roots)) in [
        (&projection, &[0usize][..]),
        (&alias, &[0usize][..]),
        (&merged, &[0usize, 1][..]),
    ]
    .into_iter()
    .enumerate()
    {
        if schedule[index] {
            check_cold_native_projection(view, catalog, &selected, Some(roots))?;
            coverage.eager_reads += 1;
        }
    }
    let cold_projection = projection.reads.read_projection.get().is_none();
    let cold_merged = merged.reads.read_projection.get().is_none();
    // Detached metadata copies retain payload observations, but never a view
    // lease or its derived read cache. Keep them alive beyond final reclamation.
    let metadata = parent.metadata_snapshot();
    let mut expected_metadata = catalog.supplied_ids(false);
    expected_metadata.insert(retired.descriptor.id);
    let retired_slot = {
        let state = inventory.0.lock().unwrap();
        let slots = state.indices.iter().filter_map(|(node, slot)| {
            matches!(node, InventoryNodeKey::Artifact(binding) if binding.artifact == retired.descriptor.id).then_some(*slot)
        }).collect::<Vec<_>>();
        prop_assert_eq!(slots.len(), 1, "one unrelated admitted row must retire");
        slots[0]
    };
    drop(parent);
    drop(left);
    drop(right);
    prop_assert!(parent_reads.upgrade().is_none());
    prop_assert!(left_reads.upgrade().is_none());
    prop_assert!(right_reads.upgrade().is_none());
    prop_assert_eq!(
        inventory.node_count(),
        catalog
            .facts(catalog.supplied_ids(false), selected.clone())
            .len()
    );
    let replacement = Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            [9; 32],
            "fixture",
            "ColdReplacement",
            BTreeMap::new(),
        ),
    ));
    let unrelated = inventory
        .admit_shared(&empty, vec![replacement.clone()])
        .unwrap();
    {
        let state = inventory.0.lock().unwrap();
        let slots = state.indices.iter().filter_map(|(node, slot)| {
            matches!(node, InventoryNodeKey::Artifact(binding) if binding.artifact == replacement.descriptor.id).then_some(*slot)
        }).collect::<Vec<_>>();
        prop_assert_eq!(slots.len(), 1);
        prop_assert_eq!(
            slots[0],
            retired_slot,
            "the history must actually reuse the retired slot"
        );
    }
    coverage.slot_reuses += 1;
    prop_assert_eq!(
        projection.reads.read_projection.get().is_none(),
        cold_projection
    );
    prop_assert_eq!(merged.reads.read_projection.get().is_none(), cold_merged);
    check_cold_native_projection(&projection, catalog, &selected, Some(&[0]))?;
    check_cold_native_projection(&merged, catalog, &selected, Some(&[0, 1]))?;
    coverage.delayed_reads += usize::from(cold_projection) + usize::from(cold_merged);
    let graph = merged.capture_graph_selection();
    let dependencies = merged.binding_dependencies();
    let recovery_inventory = ArtifactInventory::default();
    let recovered = recovery_inventory
        .recover_selection(
            &recovery_inventory.empty_view(),
            merged.entries(),
            &graph,
            &dependencies,
        )
        .unwrap();
    prop_assert!(recovered.reads.read_projection.get().is_none());
    check_cold_native_projection(&recovered, catalog, &selected, Some(&[0, 1]))?;
    prop_assert_eq!(&recovered.capture_graph_selection(), &graph);
    prop_assert_eq!(recovered.binding_dependencies(), dependencies);
    coverage.recoveries += 1;
    let projection_lease = Arc::downgrade(&projection.lease);
    drop(projection);
    prop_assert!(
        projection_lease.upgrade().is_some(),
        "the actual clone still owns its lease"
    );
    drop(alias);
    prop_assert!(projection_lease.upgrade().is_none());
    drop(merged);
    drop(unrelated);
    drop(empty);
    drop(recovered);
    prop_assert_eq!(
        inventory.node_count(),
        0,
        "metadata observation grants no graph custody"
    );
    prop_assert_eq!(recovery_inventory.node_count(), 0);
    for owner in [&inventory, &recovery_inventory] {
        let state = owner.0.lock().unwrap();
        prop_assert!(state.payloads.is_empty());
        prop_assert!(state.roots.is_empty());
    }
    prop_assert_eq!(
        metadata.artifacts.keys().copied().collect::<BTreeSet<_>>(),
        expected_metadata
    );
    prop_assert_eq!(
        metadata.selected_native_groups,
        catalog.selection(&selected)
    );
    prop_assert_eq!(
        graph
            .native_groups
            .iter()
            .map(|group| NativeGroupKey {
                artifact: group.binding.artifact,
                original_ordinal: group.original_ordinal,
            })
            .collect::<BTreeSet<_>>(),
        catalog.selection(&selected)
    );
    Ok(())
}

fn cold_later_group_history(
    catalog: &Catalog,
    eager: bool,
    coverage: &mut ColdNativeCoverage,
) -> Result<(), TestCaseError> {
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let selected = catalog.group_closure(&root_groups(0));
    let parent = inventory
        .admit_recovery_selection(
            &empty,
            catalog.entries(false, false),
            &catalog.selection(&selected),
        )
        .unwrap();
    let old_graph = parent.capture_graph_selection();
    let old_root_carrier = *old_graph
        .namespace
        .iter()
        .find(|binding| binding.artifact == catalog.originals[0].descriptor.id)
        .unwrap();
    let mut later_selected = selected.clone();
    later_selected.insert((0, 29)); // An independently authored zero-edge body.
    let later = inventory
        .admit_recovery_selection(&parent, Vec::new(), &catalog.selection(&later_selected))
        .unwrap();
    let parent_reads = Arc::downgrade(&parent.reads);
    prop_assert!(later.reads.read_projection.get().is_none());
    if eager {
        check_cold_native_projection(&later, catalog, &later_selected, None)?;
        coverage.eager_reads += 1;
    }
    drop(parent);
    prop_assert!(
        parent_reads.upgrade().is_none(),
        "retaining parent custody does not retain its read cache"
    );
    prop_assert_eq!(later.reads.read_projection.get().is_none(), !eager);
    check_cold_native_projection(&later, catalog, &later_selected, None)?;
    coverage.delayed_reads += usize::from(!eager);
    let graph = later.capture_graph_selection();
    let group = *graph
        .native_groups
        .iter()
        .find(|group| {
            group.binding.artifact == catalog.originals[0].descriptor.id
                && group.original_ordinal == 29
        })
        .unwrap();
    let carriers = later
        .binding_dependencies()
        .into_iter()
        .filter_map(|(source, target, edge)| {
            (source == ArtifactBindingNode::Group(group) && edge == ArtifactDependency::Interface)
                .then_some(target)
        })
        .collect::<BTreeSet<_>>();
    prop_assert_eq!(
        carriers,
        BTreeSet::from([ArtifactBindingNode::Artifact(old_root_carrier)])
    );
    prop_assert_ne!(
        group.binding.selection,
        old_root_carrier.selection,
        "new group retains the exact earlier carrier"
    );
    coverage.cross_scope_carriers += 1;
    drop(later);
    drop(empty);
    prop_assert_eq!(inventory.node_count(), 0);
    prop_assert!(
        !old_graph
            .native_groups
            .iter()
            .any(|group| group.original_ordinal == 29),
        "old detached selection never widens"
    );
    Ok(())
}

#[test]
fn cold_native_history_support_covers_read_and_lease_partitions() {
    let catalog = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    let mut coverage = ColdNativeCoverage::default();
    for schedule in [
        [false; 3],
        [true; 3],
        [true, false, false],
        [false, true, false],
        [false, false, true],
    ] {
        cold_native_history(&catalog, schedule, &mut coverage).unwrap();
    }
    for eager in [false, true] {
        cold_later_group_history(&catalog, eager, &mut coverage).unwrap();
    }
    assert!(coverage.eager_reads > 0 && coverage.delayed_reads > 0);
    assert_eq!(coverage.slot_reuses, 5);
    assert_eq!(coverage.recoveries, 5);
    assert_eq!(coverage.cross_scope_carriers, 2);
    eprintln!("cold native deterministic support: {coverage:?}");
}

#[test]
fn generated_cold_native_histories_preserve_exact_carriers_after_retirement() {
    use proptest::test_runner::{contextualize_config, TestRunner};
    let mut config = contextualize_config(property_config());
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::generated_cold_native_histories_preserve_exact_carriers_after_retirement"
    ));
    let cases = config.cases;
    let coverage = std::cell::RefCell::new(ColdNativeCoverage::default());
    let result = TestRunner::new(config).run(
        &(
            proptest::collection::vec(0u8..16, MODULES),
            proptest::collection::vec(0u64..8, 2 * MODULES),
            any::<[bool; 3]>(),
            any::<bool>(),
        ),
        |(masks, generations, schedule, eager)| {
            coverage.borrow_mut().callbacks += 1;
            let catalog = Catalog::new(&masks, &generations, 1);
            cold_native_history(&catalog, schedule, &mut coverage.borrow_mut())?;
            cold_later_group_history(&catalog, eager, &mut coverage.borrow_mut())?;
            coverage.borrow_mut().completed += 1;
            Ok(())
        },
    );
    eprintln!(
        "cold native campaign configured_cases={cases} coverage={:?}",
        coverage.borrow()
    );
    result.unwrap();
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
    assert!(
        coverage.recoveries > 0
            && coverage.namespace_masks > 0
            && coverage.duplicate_scope_views > 0
    );
    assert!(coverage.carrier_qualified_refusals > 0 && coverage.cross_scope_group_carriers > 0);
    eprintln!("native staged regression coverage: {coverage:?}");
}

#[test]
fn scoped_carrier_namespace_controls_projection_and_recovery() {
    let catalog = Catalog::new(&[0; MODULES], &[0; 2 * MODULES], 1);
    let history = [
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
        Op::Admit {
            parent: 1,
            root: 2,
            future: false,
            reverse: false,
            to: 3,
        },
        Op::Merge {
            left: 2,
            right: 3,
            to: 4,
        },
        Op::Read {
            from: 4,
            node: 0,
            ordinal: 3,
            all: false,
        },
        Op::Read {
            from: 4,
            node: 0,
            ordinal: 29,
            all: false,
        },
        Op::Recover {
            from: 4,
            inventory: 1,
            to: 5,
        },
        Op::Project {
            from: 5,
            nodes: vec![0],
            to: 6,
        },
        Op::Read {
            from: 6,
            node: 0,
            ordinal: 3,
            all: false,
        },
        Op::Read {
            from: 6,
            node: 0,
            ordinal: 29,
            all: false,
        },
    ];
    let coverage = run_history(&catalog, &history).unwrap();
    assert_eq!(coverage.carrier_qualified_refusals, 1);
    assert_eq!(coverage.read_refusals, 2);
    assert_eq!(coverage.reads, 2);
    assert_eq!(coverage.recoveries, 1);
    assert!(coverage.namespace_masks > 0 && coverage.duplicate_scope_views > 0);
    eprintln!("scoped carrier regression coverage: {coverage:?}");
}

#[test]
fn same_carrier_branch_groups_remain_ambiguous_after_readmission() {
    let catalog = Catalog::new(&[0; MODULES], &[0; 2 * MODULES], 1);
    let history = [
        Op::Empty {
            inventory: 0,
            to: 0,
        },
        Op::Admit {
            parent: 0,
            root: 2,
            future: false,
            reverse: false,
            to: 1,
        },
        Op::Project {
            from: 1,
            nodes: vec![0],
            to: 1,
        },
        Op::Admit {
            parent: 1,
            root: 0,
            future: false,
            reverse: false,
            to: 2,
        },
        Op::Admit {
            parent: 1,
            root: 1,
            future: true,
            reverse: false,
            to: 3,
        },
        Op::Merge {
            left: 3,
            right: 2,
            to: 4,
        },
        Op::Read {
            from: 4,
            node: 1,
            ordinal: 3,
            all: false,
        },
        Op::Admit {
            parent: 4,
            root: 0,
            future: false,
            reverse: false,
            to: 5,
        },
        Op::Read {
            from: 5,
            node: 1,
            ordinal: 3,
            all: false,
        },
        Op::Project {
            from: 5,
            nodes: vec![1],
            to: 6,
        },
        Op::Recover {
            from: 5,
            inventory: 1,
            to: 7,
        },
        Op::Read {
            from: 7,
            node: 1,
            ordinal: 3,
            all: false,
        },
    ];
    let coverage = run_history(&catalog, &history).unwrap();
    assert!(
        coverage.indexed_ambiguous > 0
            && coverage.indexed_unique > 0
            && coverage.indexed_unavailable > 0
    );
    assert_eq!(coverage.carrier_qualified_refusals, 3);
    assert_eq!(coverage.read_refusals, 3);
    assert_eq!(coverage.recoveries, 1);
    assert_eq!(coverage.refusals, 1);
    assert!(coverage.duplicate_scope_views > 0 && coverage.reclaimed > 0);
    eprintln!("same carrier branch coverage: {coverage:?}");
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn native_requirement_index_matches_flat_edge_oracle(
        raw in proptest::collection::vec((0u32..32, 0u8..4, 0u32..32, any::<bool>()), 0..96),
        queries in proptest::collection::vec(0u32..40, 0..48),
    ) {
        let edges = raw.into_iter().map(|(ordinal, owner, required, binding)| {
            let owner = ExactModuleIdentity { unit: "fixture".into(), module: format!("M{owner}") };
            let dependency = if binding {
                ArtifactDependency::NativeBinding { dependent_ordinal: ordinal, generation: u64::from(required), namespace: "value".into(), occurrence: format!("v{required}"), record_parent: None }
            } else {
                ArtifactDependency::NativeGroup { dependent_ordinal: ordinal, required_ordinal: required }
            };
            (owner, dependency)
        }).collect::<Vec<_>>();
        let indexed = index_native_requirements(edges.clone()).unwrap();
        let mut reversed = edges.clone(); reversed.reverse();
        prop_assert_eq!(&indexed, &index_native_requirements(reversed).unwrap());
        for ordinal in queries.into_iter().chain(0..40) {
            let expected = edges.iter().filter(|(_, dependency)| match dependency {
                ArtifactDependency::NativeGroup { dependent_ordinal, .. } | ArtifactDependency::NativeBinding { dependent_ordinal, .. } => *dependent_ordinal == ordinal,
                ArtifactDependency::Interface => false,
            }).cloned().collect::<BTreeSet<_>>();
            let actual = indexed.get(&ordinal).into_iter().flatten().cloned().collect::<BTreeSet<_>>();
            prop_assert_eq!(&actual, &expected);
            prop_assert_eq!(indexed.get(&ordinal).map_or(0, Vec::len), expected.len(), "duplicate edge rows are canonical sets");
        }
    }

    #[test]
    fn staged_native_histories_match_raw_fact_model(
        masks in proptest::collection::vec(0u8..16, MODULES),
        generations in proptest::collection::vec(0u64..8, 2 * MODULES),
        tail in operations(),
    ) {
        let catalog = Catalog::new(&masks, &generations, 1);
        let mut history = prefix(); history.extend(tail);
        history.extend(retained_body_history());
        let coverage = run_history(&catalog, &history)?;
        prop_assert!(coverage.admissions >= 6 && coverage.refusals >= 2);
        prop_assert!(coverage.clones > 0 && coverage.projections > 0 && coverage.merges >= 2);
        prop_assert!(coverage.cross_merges > 0 && coverage.reclaimed > 0 && coverage.read_refusals >= 3);
        prop_assert!(coverage.recoveries > 0 && coverage.namespace_masks > 0 && coverage.duplicate_scope_views > 0);
        prop_assert!(coverage.carrier_qualified_refusals > 0 && coverage.cross_scope_group_carriers > 0);
        prop_assert!(coverage.indexed_unique > 0 && coverage.indexed_unavailable > 0);
        prop_assert!(coverage.custody_only_views > 0 && coverage.custody_only_reads >= 2 && coverage.selected_retained_body > 0);
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

#[test]
fn native_selection_index_duplicate_edges_do_not_create_ambiguity() {
    let catalog = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let view = inventory
        .admit_recovery_selection(
            &empty,
            catalog.entries(false, false),
            &catalog.selection(&catalog.group_closure(&root_groups(0))),
        )
        .unwrap();
    let mut state = inventory.0.lock().expect("inventory lock");
    let projection = view.read_projection(&state);
    let (group, carrier, source, target) = projection
        .nodes
        .iter()
        .find_map(|key| {
            let InventoryNodeKey::Group(group) = key else {
                return None;
            };
            state.graph.edges(state.indices[key]).find_map(|edge| {
                let InventoryNodeKey::Artifact(carrier) = state.graph[edge.target()] else {
                    return None;
                };
                (*edge.weight() == ArtifactDependency::Interface
                    && carrier.artifact == group.binding.artifact)
                    .then_some((*group, carrier, state.indices[key], edge.target()))
            })
        })
        .expect("production admission issued a group and exact carrier");
    // Adversarial duplicate row tests the same-group any(edge) contract; it
    // neither creates another group nor changes its carrier.
    state
        .graph
        .add_edge(source, target, ArtifactDependency::Interface);
    let index = NativeSelectionIndex::build(&state, &projection.nodes);
    assert_eq!(
        index.resolve(carrier, group.original_ordinal).unwrap(),
        group
    );
    assert_eq!(
        resolve_group_binding(&state, &projection.nodes, carrier, group.original_ordinal).unwrap(),
        group
    );
}

#[test]
fn native_selection_index_is_lazy_and_does_not_retain_parent_read_cache() {
    let catalog = Catalog::new(&[0; MODULES], &[1; 2 * MODULES], 1);
    let inventory = ArtifactInventory::default();
    let empty = inventory.empty_view();
    let view = inventory
        .admit_recovery_selection(
            &empty,
            catalog.entries(false, false),
            &catalog.selection(&catalog.group_closure(&root_groups(0))),
        )
        .unwrap();
    let _ = view.descriptors();
    let _ = view.dependencies();
    assert!(view
        .reads
        .read_projection
        .get()
        .unwrap()
        .native_selection
        .get()
        .is_none());
    let selected = view
        .select_roots(vec![catalog.originals[0].descriptor.id])
        .unwrap();
    let state = inventory.0.lock().expect("inventory lock");
    let index = view.read_projection(&state).native_selection(&state);
    assert!(std::ptr::eq(
        index,
        view.clone()
            .read_projection(&state)
            .native_selection(&state)
    ));
    drop(state);
    let cache = Arc::downgrade(&view.reads);
    drop(view);
    assert!(
        cache.upgrade().is_none(),
        "child custody must not retain parent's index/read cache"
    );
    assert!(!selected.selected_native_groups().is_empty());
    drop(selected);
    drop(empty);
    assert_eq!(inventory.node_count(), 0);
}
