//! Metamorphic checks for prevalidated retained-identity hash inputs. These
//! tests do not establish import authority, product validation, or certifier
//! selection and publication.
use super::*;
use proptest::prelude::*;
use proptest::test_runner::{FileFailurePersistence, TestCaseError, TestCaseResult, TestRunner};
use std::cell::RefCell;
use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion, SymbolIdentity};

const MAX_NODES: usize = 6;
type Key = (String, String);

#[derive(Clone, Debug)]
struct LogicalNode {
    body: u8,
    skinny_interface: u8,
    product: u8,
    package: u8,
    dependencies: Vec<usize>,
    external_history: u8,
    external_binder: u8,
    declared_ordinal: u32,
    binder: u8,
}

type LogicalGraph = Vec<LogicalNode>;

fn module_key(index: usize) -> Key {
    ("model-unit".into(), format!("M{index}"))
}

fn identity(unit: &str, module: &str, occurrence: &str) -> SymbolIdentity {
    SymbolIdentity {
        unit: unit.into(),
        module: module.into(),
        namespace: "value".into(),
        occurrence: occurrence.into(),
        record_parent: None,
    }
}

fn external_owner(history: u8, binder: u8) -> PendingImportOwner {
    let mut module_version = [0; 32];
    module_version[0] = history;
    PendingImportOwner::Source {
        owner: CachedHomeOwner {
            unit: "historical-unit".into(),
            module: "External".into(),
            module_version: ModuleVersion(module_version),
            skinny_iface_sha256: [31; 32],
            product_sha256: [67; 32],
        },
        original_ordinal: 101,
        binder: identity("historical-unit", "External", &format!("binder-{binder}")),
    }
}

fn local_owner(to: usize, target: &LogicalNode) -> PendingImportOwner {
    PendingImportOwner::Source {
        owner: CachedHomeOwner {
            unit: "model-unit".into(),
            module: format!("M{to}"),
            // The selected local source is promoted in this graph, so its
            // unissued version must not feed back into the retained identity.
            module_version: ModuleVersion([0; 32]),
            skinny_iface_sha256: [target.skinny_interface; 32],
            product_sha256: [target.product; 32],
        },
        original_ordinal: target.declared_ordinal,
        binder: identity(
            "model-unit",
            &format!("M{to}"),
            &format!("value-{}", target.binder),
        ),
    }
}

fn promote(graph: &LogicalGraph, reverse_insertion: bool) -> BTreeMap<Key, PromotedModule> {
    let mut rows = Vec::new();
    for (index, logical) in graph.iter().enumerate() {
        let mut imports = logical
            .dependencies
            .iter()
            .map(|to| local_owner(*to, &graph[*to]))
            .collect::<Vec<_>>();
        // Every node has an external, historically-versioned owner, even when
        // its local dependency set is empty.
        imports.push(external_owner(
            logical.external_history,
            logical.external_binder,
        ));
        rows.push((
            module_key(index),
            PromotedModule {
                canonical_sha256: [logical.body; 32],
                product_sha256: [logical.product; 32],
                package_sha256: [logical.package; 32],
                groups: BTreeMap::from([(logical.declared_ordinal, imports)]),
            },
        ));
    }
    if reverse_insertion {
        rows.reverse();
    }
    rows.into_iter().collect()
}

fn versions(graph: &LogicalGraph) -> BTreeMap<Key, ModuleVersion> {
    module_versions(&promote(graph, false), &BTreeMap::new()).unwrap()
}

fn local_input_variant(
    graph: &LogicalGraph,
    from: usize,
    change: impl FnOnce(&mut PendingImportOwner),
) -> BTreeMap<Key, ModuleVersion> {
    let mut promoted = promote(graph, false);
    let imports = promoted
        .get_mut(&module_key(from))
        .unwrap()
        .groups
        .get_mut(&graph[from].declared_ordinal)
        .unwrap();
    let import = imports
        .iter_mut()
        .find(|import| match import {
            PendingImportOwner::Source { owner, .. } => {
                promoted_has_target(graph, owner.unit.as_str(), owner.module.as_str())
            }
            _ => false,
        })
        .expect("logical node with dependency has a local source import");
    change(import);
    module_versions(&promoted, &BTreeMap::new()).unwrap()
}

fn promoted_has_target(graph: &LogicalGraph, unit: &str, module: &str) -> bool {
    unit == "model-unit"
        && graph
            .iter()
            .enumerate()
            .any(|(index, _)| module == format!("M{index}"))
}

/// Recompute reachability from logical dependency facts, without consulting
/// the production graph traversal or its normalized identity representation.
fn affected_by(graph: &LogicalGraph, changed_node: usize) -> BTreeSet<usize> {
    (0..graph.len())
        .filter(|root| {
            let mut seen = vec![false; graph.len()];
            let mut pending = vec![*root];
            while let Some(node) = pending.pop() {
                if seen[node] {
                    continue;
                }
                seen[node] = true;
                pending.extend(graph[node].dependencies.iter().copied());
            }
            seen[changed_node]
        })
        .collect()
}

fn changed_versions(
    before: &BTreeMap<Key, ModuleVersion>,
    after: &BTreeMap<Key, ModuleVersion>,
) -> BTreeSet<usize> {
    (0..before.len())
        .filter(|index| before[&module_key(*index)] != after[&module_key(*index)])
        .collect()
}

fn sample_graph(masks: &[u8], bodies: &[u8], histories: &[u8], binders: &[u8]) -> LogicalGraph {
    let count = masks.len();
    (0..count)
        .map(|from| {
            let mut dependencies = Vec::new();
            for to in from + 1..count {
                let bit = to - from - 1;
                if masks[from] & (1 << bit) != 0 {
                    dependencies.push(to);
                }
            }
            LogicalNode {
                body: bodies[from],
                skinny_interface: bodies[from].wrapping_add(13),
                product: bodies[from].wrapping_add(23),
                package: bodies[from].wrapping_add(29),
                dependencies,
                external_history: histories[from],
                external_binder: binders[from],
                declared_ordinal: 17 + from as u32,
                binder: binders[from],
            }
        })
        .collect()
}

fn property_config() -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 128;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum MutationFact {
    CanonicalSeal,
    ProductSeal,
    PackageSeal,
    ExternalVersion,
    ExternalBinder,
    RepresentationOrder,
}

#[derive(Default, serde::Serialize)]
struct MutationCoverage {
    evaluations: usize,
    expected_changed_roots: usize,
    expected_unchanged_roots: usize,
    observed_changed_roots: usize,
    observed_noops: usize,
    semantic_rejections: usize,
}

#[derive(Default, serde::Serialize)]
struct ObservedCoverage {
    // Runner callbacks include persisted replay and shrinking, not just newly
    // generated cases. These counters describe inputs observed in this process.
    inputs: usize,
    completed_inputs: usize,
    nodes: [usize; MAX_NODES + 1],
    local_edges: [usize; MAX_NODES * (MAX_NODES - 1) / 2 + 1],
    maximum_local_depth: [usize; MAX_NODES],
    inputs_with_shared_descendants: usize,
    shared_descendants: usize,
    inputs_with_isolated_nodes: usize,
    isolated_nodes: usize,
    canonical_mutation_target_positions: [usize; MAX_NODES],
    untargeted_canonical_nodes: usize,
    semantic_rejections: usize,
    mutations: BTreeMap<MutationFact, MutationCoverage>,
}

impl ObservedCoverage {
    fn observe_graph(&mut self, graph: &LogicalGraph) {
        self.inputs += 1;
        self.nodes[graph.len()] += 1;
        let mut incoming = vec![0; graph.len()];
        let mut depth = vec![0; graph.len()];
        let mut edges = 0;
        for from in (0..graph.len()).rev() {
            for to in &graph[from].dependencies {
                incoming[*to] += 1;
                edges += 1;
                depth[from] = depth[from].max(1 + depth[*to]);
            }
        }
        self.local_edges[edges] += 1;
        self.maximum_local_depth[*depth.iter().max().unwrap()] += 1;
        let shared = incoming.iter().filter(|count| **count > 1).count();
        self.shared_descendants += shared;
        self.inputs_with_shared_descendants += usize::from(shared > 0);
        let isolated = graph
            .iter()
            .enumerate()
            .filter(|(index, node)| incoming[*index] == 0 && node.dependencies.is_empty())
            .count();
        self.isolated_nodes += isolated;
        self.inputs_with_isolated_nodes += usize::from(isolated > 0);
    }

    fn check_mutation(
        &mut self,
        fact: MutationFact,
        before: &BTreeMap<Key, ModuleVersion>,
        candidate: CertResult<BTreeMap<Key, ModuleVersion>>,
        expected: BTreeSet<usize>,
    ) -> TestCaseResult {
        let observed = self.mutations.entry(fact).or_default();
        observed.evaluations += 1;
        observed.expected_changed_roots += expected.len();
        observed.expected_unchanged_roots += before.len() - expected.len();
        let after = match candidate {
            Ok(after) => after,
            Err(error) => {
                observed.semantic_rejections += 1;
                self.semantic_rejections += 1;
                return Err(TestCaseError::fail(format!("{fact:?} rejected: {error:?}")));
            }
        };
        let actual = changed_versions(before, &after);
        observed.observed_changed_roots += actual.len();
        observed.observed_noops += usize::from(actual.is_empty());
        prop_assert_eq!(actual, expected, "mutation fact={:?}", fact);
        Ok(())
    }
}

fn check_graph(graph: &LogicalGraph, coverage: &mut ObservedCoverage) -> TestCaseResult {
    coverage.observe_graph(graph);
    let promoted = promote(graph, false);
    prop_assert!(promoted
        .values()
        .all(|node| node.groups.values().flatten().next().is_some()));
    let before = module_versions(&promoted, &BTreeMap::new()).map_err(|error| {
        coverage.semantic_rejections += 1;
        TestCaseError::fail(format!("initial graph rejected: {error:?}"))
    })?;
    // Distinct positions avoid counting the middle/last node twice at size two.
    // This deliberately samples positions rather than mutating every node.
    let targets = BTreeSet::from([0, graph.len() / 2, graph.len() - 1]);
    coverage.untargeted_canonical_nodes += graph.len() - targets.len();
    for target in targets {
        coverage.canonical_mutation_target_positions[target] += 1;
        let expected = affected_by(graph, target);
        for fact in [
            MutationFact::CanonicalSeal,
            MutationFact::ProductSeal,
            MutationFact::PackageSeal,
            MutationFact::ExternalVersion,
        ] {
            let mut changed = graph.clone();
            let node = &mut changed[target];
            match fact {
                MutationFact::CanonicalSeal => node.body = node.body.wrapping_add(1),
                MutationFact::ProductSeal => node.product = node.product.wrapping_add(1),
                MutationFact::PackageSeal => node.package = node.package.wrapping_add(1),
                MutationFact::ExternalVersion => {
                    node.external_history = node.external_history.wrapping_add(1)
                }
                MutationFact::ExternalBinder | MutationFact::RepresentationOrder => unreachable!(),
            }
            coverage.check_mutation(
                fact,
                &before,
                module_versions(&promote(&changed, false), &BTreeMap::new()),
                expected.clone(),
            )?;
        }
    }
    // Every node has an external import, including graphs with no local edges.
    let target = graph.len() / 2;
    let mut changed = graph.clone();
    changed[target].external_binder = changed[target].external_binder.wrapping_add(1);
    coverage.check_mutation(
        MutationFact::ExternalBinder,
        &before,
        module_versions(&promote(&changed, false), &BTreeMap::new()),
        affected_by(graph, target),
    )?;
    // BTreeMap normalizes insertion order before traversal. This is a
    // representation-order control, not an arbitrary traversal-order claim.
    coverage.check_mutation(
        MutationFact::RepresentationOrder,
        &before,
        module_versions(&promote(graph, true), &BTreeMap::new()),
        BTreeSet::new(),
    )?;
    coverage.completed_inputs += 1;
    Ok(())
}

#[test]
fn retained_graph_chain_diamond_shares_descendants_and_isolates_disconnected_nodes() {
    let graph = vec![
        LogicalNode {
            body: 1,
            skinny_interface: 12,
            product: 22,
            package: 3,
            dependencies: vec![1, 2],
            external_history: 1,
            external_binder: 1,
            declared_ordinal: 17,
            binder: 1,
        },
        LogicalNode {
            body: 2,
            skinny_interface: 13,
            product: 23,
            package: 4,
            dependencies: vec![3],
            external_history: 2,
            external_binder: 2,
            declared_ordinal: 18,
            binder: 2,
        },
        LogicalNode {
            body: 3,
            skinny_interface: 14,
            product: 24,
            package: 5,
            dependencies: vec![3],
            external_history: 3,
            external_binder: 3,
            declared_ordinal: 19,
            binder: 3,
        },
        LogicalNode {
            body: 4,
            skinny_interface: 15,
            product: 25,
            package: 6,
            dependencies: vec![4],
            external_history: 4,
            external_binder: 4,
            declared_ordinal: 20,
            binder: 4,
        },
        LogicalNode {
            body: 5,
            skinny_interface: 16,
            product: 26,
            package: 7,
            dependencies: vec![],
            external_history: 5,
            external_binder: 5,
            declared_ordinal: 21,
            binder: 5,
        },
        LogicalNode {
            body: 6,
            skinny_interface: 17,
            product: 27,
            package: 8,
            dependencies: vec![],
            external_history: 6,
            external_binder: 6,
            declared_ordinal: 22,
            binder: 6,
        },
    ];
    let before = versions(&graph);
    let mut changed = graph.clone();
    changed[4].body = 41;
    assert_eq!(
        changed_versions(&before, &versions(&changed)),
        affected_by(&graph, 4)
    );
    assert_eq!(affected_by(&graph, 4), BTreeSet::from([0, 1, 2, 3, 4]));

    let mut changed = graph.clone();
    changed[3].external_history = 19;
    assert_eq!(
        changed_versions(&before, &versions(&changed)),
        affected_by(&graph, 3)
    );
    assert_eq!(affected_by(&graph, 3), BTreeSet::from([0, 1, 2, 3]));

    let mut changed = graph.clone();
    changed[3].dependencies.clear();
    assert_eq!(
        changed_versions(&before, &versions(&changed)),
        affected_by(&graph, 3)
    );

    let reverse = promote(&graph, true);
    assert_eq!(module_versions(&reverse, &BTreeMap::new()).unwrap(), before);
}

#[test]
fn retained_graph_exhausts_three_node_dags_and_local_import_mutations() {
    let mut edge_count = 0;
    let mut shared_descendant_topologies = 0;
    let mut empty_topologies = 0;
    let mut support = ObservedCoverage::default();
    for topology in 0u8..8 {
        let graph = vec![
            LogicalNode {
                body: 1,
                skinny_interface: 31,
                product: 41,
                package: 21,
                dependencies: [1usize, 2]
                    .into_iter()
                    .enumerate()
                    .filter_map(|(bit, target)| (topology & (1 << bit) != 0).then_some(target))
                    .collect(),
                external_history: 1,
                external_binder: 4,
                declared_ordinal: 17,
                binder: 7,
            },
            LogicalNode {
                body: 2,
                skinny_interface: 32,
                product: 42,
                package: 22,
                dependencies: (topology & 4 != 0).then_some(2).into_iter().collect(),
                external_history: 2,
                external_binder: 5,
                declared_ordinal: 18,
                binder: 8,
            },
            LogicalNode {
                body: 3,
                skinny_interface: 33,
                product: 43,
                package: 23,
                dependencies: vec![],
                external_history: 3,
                external_binder: 6,
                declared_ordinal: 19,
                binder: 9,
            },
        ];
        check_graph(&graph, &mut support).unwrap();
        edge_count += graph
            .iter()
            .map(|node| node.dependencies.len())
            .sum::<usize>();
        let mut incoming = [0; 3];
        for node in &graph {
            for target in &node.dependencies {
                incoming[*target] += 1;
            }
        }
        if incoming.iter().any(|owners| *owners > 1) {
            shared_descendant_topologies += 1;
        }
        if graph.iter().all(|node| node.dependencies.is_empty()) {
            empty_topologies += 1;
        }
        let before = versions(&graph);
        let promoted = promote(&graph, false);
        for (from, node) in promoted.values().enumerate() {
            let local_imports = node
                .groups
                .values()
                .flatten()
                .filter_map(|import| match import {
                    PendingImportOwner::Source { owner, .. }
                        if promoted.contains_key(&(owner.unit.clone(), owner.module.clone())) =>
                    {
                        Some(owner)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(node.groups.values().any(|imports| !imports.is_empty()));
            assert!(local_imports
                .iter()
                .all(|owner| owner.module_version == ModuleVersion([0; 32])));
            if !graph[from].dependencies.is_empty() {
                let expected = affected_by(&graph, from);
                // These isolated mutations probe hash sensitivity at the
                // semantic input boundary; they are not claims about accepted
                // compiler receipts after owner validation.
                let ordinal_changed = local_input_variant(&graph, from, |import| {
                    if let PendingImportOwner::Source {
                        original_ordinal, ..
                    } = import
                    {
                        *original_ordinal += 1;
                    }
                });
                assert_eq!(changed_versions(&before, &ordinal_changed), expected);

                let binder_changed = local_input_variant(&graph, from, |import| {
                    if let PendingImportOwner::Source { binder, .. } = import {
                        binder.occurrence.push_str("-variant");
                    }
                });
                assert_eq!(changed_versions(&before, &binder_changed), expected);
            }
        }

        for from in 0..graph.len() {
            for to in from + 1..graph.len() {
                let mut changed = graph.clone();
                if let Some(position) = changed[from]
                    .dependencies
                    .iter()
                    .position(|item| *item == to)
                {
                    changed[from].dependencies.remove(position);
                } else {
                    changed[from].dependencies.push(to);
                    changed[from].dependencies.sort_unstable();
                }
                assert_eq!(
                    changed_versions(&before, &versions(&changed)),
                    affected_by(&graph, from),
                    "topology={topology}, changed import M{from}->M{to}"
                );
            }
        }
    }
    assert_eq!(edge_count, 12);
    assert_eq!(shared_descendant_topologies, 2);
    assert_eq!(empty_topologies, 1);
    // These exact partitions come from the eight deterministic topologies;
    // random campaign frequencies never gate correctness.
    assert_eq!(support.inputs, 8);
    assert_eq!(support.completed_inputs, 8);
    assert_eq!(&support.local_edges[..4], &[1, 3, 3, 1]);
    assert_eq!(&support.maximum_local_depth[..3], &[1, 5, 2]);
    assert_eq!(support.inputs_with_shared_descendants, 2);
    assert_eq!(support.inputs_with_isolated_nodes, 4);
    assert_eq!(support.isolated_nodes, 6);
    assert_eq!(
        &support.canonical_mutation_target_positions[..3],
        &[8, 8, 8]
    );
    assert_eq!(support.untargeted_canonical_nodes, 0);
    assert_eq!(support.semantic_rejections, 0);
    for fact in [
        MutationFact::CanonicalSeal,
        MutationFact::ProductSeal,
        MutationFact::PackageSeal,
        MutationFact::ExternalVersion,
        MutationFact::ExternalBinder,
    ] {
        assert!(support.mutations[&fact].expected_changed_roots > 0);
        assert!(support.mutations[&fact].expected_unchanged_roots > 0);
        assert_eq!(support.mutations[&fact].observed_noops, 0);
    }
    assert_eq!(
        support.mutations[&MutationFact::RepresentationOrder].observed_noops,
        8
    );
    eprintln!(
        "handcrafted exhaustive retained graph support: topologies=8, edges={edge_count}, shared_descendant_topologies={shared_descendant_topologies}, empty_topologies={empty_topologies}, external_history_versions=3"
    );
    eprintln!(
        "retained_graph_deterministic_support={}",
        serde_json::to_string(&support).unwrap()
    );
}

#[test]
fn retained_graph_identity_tracks_logical_dependency_facts() {
    let mut config = property_config();
    // Match proptest!'s Cargo source/test identity; native Direct persistence
    // still uses the owning package's declared path rather than this source path.
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::retained_graph_identity_tracks_logical_dependency_facts"
    ));
    let configured_cases = config.cases;
    let configured_max_shrink_iters = config.max_shrink_iters;
    let configuration = format!("{config:?}");
    let strategy = (
        2usize..=MAX_NODES,
        proptest::collection::vec(any::<u8>(), MAX_NODES),
        proptest::collection::vec(1u8..=240, MAX_NODES),
        proptest::collection::vec(1u8..=240, MAX_NODES),
        proptest::collection::vec(1u8..=240, MAX_NODES),
    );
    let coverage = RefCell::new(ObservedCoverage::default());
    let mut runner = TestRunner::new(config);
    let result = runner.run(&strategy, |(count, masks, bodies, histories, binders)| {
        let graph = sample_graph(
            &masks[..count],
            &bodies[..count],
            &histories[..count],
            &binders[..count],
        );
        check_graph(&graph, &mut coverage.borrow_mut())
    });
    // Emit on failure too. Counts are callback evaluations in this process,
    // including persisted replay/shrinking, not a claim about fresh random cases.
    eprintln!(
        "retained_graph_campaign={}",
        serde_json::json!({
            "configured_cases": configured_cases,
            "configured_max_shrink_iters": configured_max_shrink_iters,
            "configuration": configuration,
            "observation_scope": "runner callbacks in this process, including replay and shrinking",
            "observed": &*coverage.borrow(),
        })
    );
    if let Err(error) = result {
        panic!("retained graph property failed: {error}");
    }
}
