use super::*;
use proptest::prelude::*;
use proptest::test_runner::{FileFailurePersistence, ProptestConfig};
use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion, SymbolIdentity};

const MAX_NODES: usize = 6;
type Key = (String, String);

#[derive(Clone, Debug)]
struct LogicalNode {
    body: u8,
    interface: u8,
    dependencies: Vec<usize>,
    external_history: u8,
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

fn external_owner(history: u8) -> PendingImportOwner {
    let mut module_version = [0; 32];
    module_version[0] = history;
    let mut interface = [0; 32];
    interface[0] = history.wrapping_add(31);
    let mut product = [0; 32];
    product[0] = history.wrapping_add(67);
    PendingImportOwner::Source {
        owner: CachedHomeOwner {
            unit: "historical-unit".into(),
            module: "External".into(),
            module_version: ModuleVersion(module_version),
            skinny_iface_sha256: interface,
            product_sha256: product,
        },
        original_ordinal: 101 + u32::from(history),
        binder: identity("historical-unit", "External", &format!("v{history}")),
    }
}

fn local_owner(from: usize, to: usize, binder: u8) -> PendingImportOwner {
    PendingImportOwner::Source {
        owner: CachedHomeOwner {
            unit: "model-unit".into(),
            module: format!("M{to}"),
            // The selected local source is promoted in this graph, so its
            // unissued version must not feed back into the retained identity.
            module_version: ModuleVersion([0; 32]),
            skinny_iface_sha256: [to as u8; 32],
            product_sha256: [from as u8; 32],
        },
        original_ordinal: 17 + to as u32,
        binder: identity(
            "model-unit",
            &format!("M{to}"),
            &format!("edge-{from}-{to}-binder-{binder}"),
        ),
    }
}

fn promote(graph: &LogicalGraph, reverse_insertion: bool) -> BTreeMap<Key, PromotedModule> {
    let mut rows = Vec::new();
    for (index, logical) in graph.iter().enumerate() {
        let mut imports = logical
            .dependencies
            .iter()
            .map(|to| local_owner(index, *to, logical.binder))
            .collect::<Vec<_>>();
        // Every node has an external, historically-versioned owner, even when
        // its local dependency set is empty.
        imports.push(external_owner(logical.external_history));
        rows.push((
            module_key(index),
            PromotedModule {
                canonical_sha256: [logical.body; 32],
                product_sha256: [logical.interface; 32],
                package_sha256: [logical.interface.wrapping_add(1); 32],
                groups: BTreeMap::from([(17, imports)]),
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
                // Keep at least a chain; the remaining bits produce branches
                // and shared descendants while preserving a bounded DAG.
                if bit == 0 || masks[from] & (1 << (bit - 1)) != 0 {
                    dependencies.push(to);
                }
            }
            LogicalNode {
                body: bodies[from],
                interface: bodies[from].wrapping_add(13),
                dependencies,
                external_history: histories[from],
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

#[test]
fn retained_graph_chain_diamond_shares_descendants_and_isolates_disconnected_nodes() {
    let graph = vec![
        LogicalNode {
            body: 1,
            interface: 2,
            dependencies: vec![1, 2],
            external_history: 1,
            binder: 1,
        },
        LogicalNode {
            body: 2,
            interface: 3,
            dependencies: vec![3],
            external_history: 2,
            binder: 2,
        },
        LogicalNode {
            body: 3,
            interface: 4,
            dependencies: vec![3],
            external_history: 3,
            binder: 3,
        },
        LogicalNode {
            body: 4,
            interface: 5,
            dependencies: vec![4],
            external_history: 4,
            binder: 4,
        },
        LogicalNode {
            body: 5,
            interface: 6,
            dependencies: vec![],
            external_history: 5,
            binder: 5,
        },
        LogicalNode {
            body: 6,
            interface: 7,
            dependencies: vec![],
            external_history: 6,
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
fn retained_graph_fixed_seed_samples_cover_shared_and_historical_inputs() {
    let mut state = 0x6d6f_6465_6c_u64;
    let mut edge_count = 0;
    let mut shared_descendant_cases = 0;
    let mut distinct_history_cases = 0;
    for _ in 0..32 {
        let mut masks = [0; MAX_NODES];
        let mut bodies = [0; MAX_NODES];
        let mut histories = [0; MAX_NODES];
        let mut binders = [0; MAX_NODES];
        for index in 0..MAX_NODES {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            masks[index] = (state >> 32) as u8;
            bodies[index] = ((state >> 24) as u8).max(1);
            histories[index] = (state as u8).wrapping_add(index as u8);
            binders[index] = ((state >> 16) as u8).max(1);
        }
        let graph = sample_graph(&masks, &bodies, &histories, &binders);
        let mut incoming = [0; MAX_NODES];
        for node in &graph {
            edge_count += node.dependencies.len();
            for target in &node.dependencies {
                incoming[*target] += 1;
            }
        }
        shared_descendant_cases += usize::from(incoming.iter().any(|owners| *owners > 1));
        distinct_history_cases += usize::from(histories.iter().collect::<BTreeSet<_>>().len() > 1);

        let current = versions(&graph);
        let target = (state as usize) % MAX_NODES;
        let mut changed = graph.clone();
        changed[target].interface = changed[target].interface.wrapping_add(1);
        assert_eq!(
            changed_versions(&current, &versions(&changed)),
            affected_by(&graph, target)
        );
    }
    assert!(edge_count > 32);
    assert!(shared_descendant_cases > 0);
    assert!(distinct_history_cases > 0);
    eprintln!(
        "retained graph sample coverage: samples=32, edges={edge_count}, shared_descendant_cases={shared_descendant_cases}, distinct_history_cases={distinct_history_cases}"
    );
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn retained_graph_identity_tracks_logical_dependency_facts(
        count in 2usize..=MAX_NODES,
        masks in proptest::collection::vec(any::<u8>(), MAX_NODES),
        bodies in proptest::collection::vec(1u8..=240, MAX_NODES),
        histories in proptest::collection::vec(1u8..=240, MAX_NODES),
        binders in proptest::collection::vec(1u8..=240, MAX_NODES),
    ) {
        let mut graph = sample_graph(&masks[..count], &bodies[..count], &histories[..count], &binders[..count]);
        let before = versions(&graph);

        // Body, interface, and external historical owner changes affect
        // precisely the roots that can reach the changed logical node.
        for target in [0, count / 2, count - 1] {
            let expected = affected_by(&graph, target);
            let mut changed = graph.clone();
            changed[target].body = changed[target].body.wrapping_add(1);
            prop_assert_eq!(changed_versions(&before, &versions(&changed)), expected.clone());

            let mut changed = graph.clone();
            changed[target].interface = changed[target].interface.wrapping_add(1);
            prop_assert_eq!(changed_versions(&before, &versions(&changed)), expected.clone());

            let mut changed = graph.clone();
            changed[target].external_history = changed[target].external_history.wrapping_add(1);
            prop_assert_eq!(changed_versions(&before, &versions(&changed)), expected);
        }

        // Changing an import binder changes that node's identity and all
        // ancestors that retain it, while independent components stay equal.
        let target = count / 2;
        let expected = affected_by(&graph, target);
        graph[target].binder = graph[target].binder.wrapping_add(1);
        prop_assert_eq!(changed_versions(&before, &versions(&graph)), expected);

        let forward = promote(&graph, false);
        let reverse = promote(&graph, true);
        prop_assert_eq!(module_versions(&forward, &BTreeMap::new()).unwrap(), module_versions(&reverse, &BTreeMap::new()).unwrap());
        prop_assert!(forward.values().all(|node| node.groups.values().flatten().next().is_some()));
    }
}
