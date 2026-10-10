//! Transport facts only: these plans do not claim compiler-issued authority.
//! Recovery authority and native custody are exercised by the owning recovery tests.
use super::*;

#[derive(Debug, Eq, PartialEq)]
struct PlanFacts {
    nodes: BTreeSet<ArtifactBindingNode>,
    edges: BTreeSet<BoundEdge>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct ContextFacts {
    plans: Vec<ArtifactSelectionId>,
    projection: serde_json::Value,
}

#[derive(Debug, Eq, PartialEq)]
struct ExpandedFacts {
    contexts: BTreeMap<String, ContextFacts>,
    plans: BTreeMap<ArtifactSelectionId, PlanFacts>,
}

fn context_facts(selection: &ArtifactGraphSelection) -> ContextFacts {
    let mut projection = selection.clone();
    projection.selections.clear();
    ContextFacts {
        plans: selection
            .selections
            .iter()
            .map(|plan| plan.selection)
            .collect(),
        projection: serde_json::to_value(projection).unwrap(),
    }
}

fn selections(graph: &RecoveryGraphWire) -> BTreeMap<String, &ArtifactGraphSelection> {
    fn add_roles<'a>(
        rows: &mut BTreeMap<String, &'a ArtifactGraphSelection>,
        prefix: &str,
        roles: &'a [CompilerInputRole],
    ) {
        for (index, role) in roles.iter().enumerate() {
            if let CompilerInputRole::PublishedSourceOriginal { selection, .. } = role {
                rows.insert(
                    format!("{prefix}/role/{index}"),
                    selection.graph_selection(),
                );
            }
        }
    }
    let mut rows = BTreeMap::new();
    for (index, node) in graph.nodes.iter().enumerate() {
        let prefix = format!("node/{index}");
        rows.insert(format!("{prefix}/outer"), &node.graph_selection);
        add_roles(&mut rows, &prefix, &node.compiler_roles);
    }
    for (index, surface) in graph.public_surfaces.iter().enumerate() {
        let prefix = format!("surface/{index}");
        rows.insert(
            format!("{prefix}/outer"),
            &surface.compiler_context.graph_selection,
        );
        add_roles(&mut rows, &prefix, &surface.compiler_context.compiler_roles);
    }
    rows
}

// Observe original typed tuples and every projection field, without consulting
// dictionary indices, codec collection, or the recovery validator.
fn expanded_facts(graph: &RecoveryGraphWire) -> ExpandedFacts {
    let mut facts = ExpandedFacts {
        contexts: BTreeMap::new(),
        plans: BTreeMap::new(),
    };
    let mut observed_owners: BTreeMap<ArtifactSelectionId, &Arc<ArtifactSelectionPlan>> =
        BTreeMap::new();
    for (path, selection) in selections(graph) {
        for plan in &selection.selections {
            if let Some(previous) = observed_owners.get(&plan.selection) {
                assert_eq!(previous.as_ref(), plan.as_ref());
            } else {
                observed_owners.insert(plan.selection, plan);
                let nodes = plan.nodes.iter().copied().collect::<BTreeSet<_>>();
                let edges = plan.dependencies.iter().cloned().collect::<BTreeSet<_>>();
                assert_eq!(nodes.len(), plan.nodes.len());
                assert_eq!(edges.len(), plan.dependencies.len());
                facts
                    .plans
                    .insert(plan.selection, PlanFacts { nodes, edges });
            }
        }
        facts.contexts.insert(path, context_facts(selection));
    }
    facts
}

fn assert_exact_facts(original: &RecoveryGraphWire, restored: &RecoveryGraphWire) {
    assert_eq!(expanded_facts(original), expanded_facts(restored));
    let restored_selections = selections(restored);
    let mut checked_json = BTreeSet::new();
    for (path, expected) in selections(original) {
        let actual = restored_selections[&path];
        assert_eq!(expected, actual, "selection at {path}");
        // The uncompressed serializer is a second observation boundary. Repeated
        // large contexts have already been compared above; serialize each exact
        // selection value once to avoid making the oracle itself quadratic.
        let key = serde_json::to_vec(&context_facts(expected)).unwrap();
        if checked_json.insert(key) {
            assert_eq!(
                serde_json::to_value(expected).unwrap(),
                serde_json::to_value(actual).unwrap(),
                "uncompressed selection at {path}"
            );
        }
    }
    assert_eq!(original.nodes.len(), restored.nodes.len());
    for (expected, actual) in original.nodes.iter().zip(&restored.nodes) {
        assert_eq!(expected.compiler_roles, actual.compiler_roles);
    }
    for (expected, actual) in original
        .public_surfaces
        .iter()
        .zip(&restored.public_surfaces)
    {
        assert_eq!(
            expected.compiler_context.compiler_roles,
            actual.compiler_context.compiler_roles
        );
    }
}

fn assert_shared_plan_owners(graph: &RecoveryGraphWire) -> usize {
    let mut owners: BTreeMap<ArtifactSelectionId, &Arc<ArtifactSelectionPlan>> = BTreeMap::new();
    let mut shared = 0;
    for selection in selections(graph).values() {
        for plan in &selection.selections {
            if let Some(previous) = owners.get(&plan.selection) {
                assert!(
                    Arc::ptr_eq(previous, plan),
                    "plan {:?} lost sharing",
                    plan.selection
                );
                shared += 1;
            } else {
                owners.insert(plan.selection, plan);
            }
        }
    }
    let distinct = owners.values().collect::<Vec<_>>();
    for pair in distinct.windows(2) {
        assert!(
            !Arc::ptr_eq(pair[0], pair[1]),
            "distinct plan IDs were aliased"
        );
    }
    shared
}

fn digest(value: u64) -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&value.to_be_bytes());
    bytes
}

fn binding(payload: u64, scope: u64) -> ArtifactBinding {
    ArtifactBinding {
        artifact: ArtifactId(digest(payload)),
        selection: ArtifactSelectionId(digest(scope)),
    }
}

fn group(payload: u64, scope: u64, ordinal: u32) -> ArtifactBindingNode {
    ArtifactBindingNode::Group(NativeGroupBinding {
        binding: binding(payload, scope),
        original_ordinal: ordinal,
    })
}

fn plan(
    id: u64,
    nodes: impl IntoIterator<Item = ArtifactBindingNode>,
    edges: impl IntoIterator<Item = BoundEdge>,
) -> Arc<ArtifactSelectionPlan> {
    Arc::new(ArtifactSelectionPlan {
        selection: ArtifactSelectionId(digest(id)),
        nodes: nodes
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        dependencies: edges
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    })
}

fn selection(plans: Vec<Arc<ArtifactSelectionPlan>>) -> ArtifactGraphSelection {
    let mut plans = plans;
    plans.sort_by_key(|plan| plan.selection);
    let nodes = plans
        .iter()
        .flat_map(|plan| plan.nodes.iter().copied())
        .collect::<BTreeSet<_>>();
    let artifacts = nodes
        .iter()
        .filter_map(|node| match node {
            ArtifactBindingNode::Artifact(binding) => Some(*binding),
            ArtifactBindingNode::Group(_) => None,
        })
        .take(2)
        .collect::<Vec<_>>();
    ArtifactGraphSelection {
        selections: plans,
        bindings: artifacts.clone(),
        namespace: artifacts.iter().take(1).copied().collect(),
        roots: artifacts.iter().rev().take(1).copied().map(ArtifactBindingNode::Artifact).collect(),
        native_groups: nodes
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(*group),
                ArtifactBindingNode::Artifact(_) => None,
            })
            .collect(),
        native_custody: artifacts,
    }
}

fn role(selection: ArtifactGraphSelection) -> CompilerInputRole {
    CompilerInputRole::PublishedSourceOriginal {
        interface: ArtifactId(digest(90)),
        original: ArtifactId(digest(91)),
        source_revision: "transport-only-source-revision".into(),
        original_input_identity: "transport-only-original-input".into(),
        selection: ExactArtifactSelection::from_recovered_graph_selection(selection),
        selection_sha256: [0x71; 32],
    }
}

fn graph_with_contexts(contexts: &[ArtifactGraphSelection]) -> RecoveryGraphWire {
    assert!(!contexts.is_empty());
    let mut graph = super::super::tests::fixture();
    let node = graph.nodes[0].clone();
    let surface = graph.public_surfaces[0].clone();
    graph.nodes.clear();
    graph.public_surfaces.clear();
    for (index, context) in contexts.iter().enumerate() {
        let nested = contexts[(index + 1) % contexts.len()].clone();
        let mut next_node = node.clone();
        next_node.id = Generation(index as u64 + 1);
        next_node.graph_selection = context.clone();
        next_node.compiler_roles = vec![role(nested.clone())];
        graph.nodes.push(next_node);
        let mut next_surface = surface.clone();
        next_surface.owner = RecoveryPublicOwner {
            path: format!("codec-{index}"),
            incarnation: 1,
        };
        next_surface.compiler_context.graph_selection = nested;
        next_surface.compiler_context.compiler_roles = vec![role(context.clone())];
        graph.public_surfaces.push(next_surface);
    }
    graph.high_water = Generation(contexts.len() as u64);
    graph.checksum.clear();
    graph
}

fn raw(graph: &RecoveryGraphWire) -> EncodedGraph {
    serde_json::from_slice(&serde_json::to_vec(graph).unwrap()).unwrap()
}

fn sign_raw(raw: &mut EncodedGraph) {
    raw.checksum.clear();
    let mut bytes = b"tidepool-recovery-graph-v11\0".to_vec();
    bytes.extend(serde_json::to_vec(raw).unwrap());
    raw.checksum = blake3::hash(&bytes).to_hex().to_string();
}

fn roundtrip(graph: &RecoveryGraphWire) -> (Vec<u8>, RecoveryGraphWire) {
    let mut encoded = raw(graph);
    sign_raw(&mut encoded);
    let bytes = serde_json::to_vec(&encoded).unwrap();
    let restored: RecoveryGraphWire = serde_json::from_slice(&bytes).unwrap();
    assert_exact_facts(graph, &restored);
    (bytes, restored)
}

fn rich_plan(id: u64) -> Arc<ArtifactSelectionPlan> {
    let a1 = ArtifactBindingNode::Artifact(binding(1, 1));
    let a2 = ArtifactBindingNode::Artifact(binding(1, 2));
    let b = ArtifactBindingNode::Artifact(binding(2, 1));
    let c = ArtifactBindingNode::Artifact(binding(3, 1));
    let g0 = group(1, 2, 0);
    let g1 = group(1, 2, 1);
    plan(
        id,
        [a1, a2, b, c, g0, g1],
        [
            (a2, b, ArtifactDependency::Interface),
            (a2, c, ArtifactDependency::Interface),
            (b, a1, ArtifactDependency::Interface),
            (c, a1, ArtifactDependency::Interface),
            // The group carrier targets inherited S1 rather than its own S2.
            (g0, a1, ArtifactDependency::Interface),
            (
                g0,
                g1,
                ArtifactDependency::NativeGroup {
                    dependent_ordinal: 0,
                    required_ordinal: 1,
                },
            ),
            (
                g1,
                g0,
                ArtifactDependency::NativeGroup {
                    dependent_ordinal: 1,
                    required_ordinal: 0,
                },
            ),
            (
                g1,
                g1,
                ArtifactDependency::NativeGroup {
                    dependent_ordinal: 1,
                    required_ordinal: 1,
                },
            ),
            (
                g0,
                b,
                ArtifactDependency::NativeBinding {
                    dependent_ordinal: 0,
                    generation: 43,
                    namespace: "value".into(),
                    occurrence: "field".into(),
                    record_parent: Some("Record".into()),
                },
            ),
            (
                g1,
                c,
                ArtifactDependency::NativeBinding {
                    dependent_ordinal: 1,
                    generation: 44,
                    namespace: "constructor".into(),
                    occurrence: "MkRecord".into(),
                    record_parent: None,
                },
            ),
        ],
    )
}

#[test]
fn scoped_diamond_recursive_groups_and_nested_source_roles_preserve_exact_facts() {
    let complete = rich_plan(11);
    let earlier = plan(
        12,
        complete.nodes.iter().copied(),
        complete.dependencies[..5].iter().cloned(),
    );
    let graph = graph_with_contexts(&[
        selection(vec![Arc::clone(&complete)]),
        selection(vec![Arc::clone(&earlier)]),
        selection(vec![complete, earlier]),
    ]);
    let facts = expanded_facts(&graph);
    let complete = &facts.plans[&ArtifactSelectionId(digest(11))];
    assert_eq!(complete.nodes.len(), 6);
    assert_eq!(complete.edges.len(), 10);
    assert!(complete
        .nodes
        .contains(&ArtifactBindingNode::Artifact(binding(1, 1))));
    assert!(complete
        .nodes
        .contains(&ArtifactBindingNode::Artifact(binding(1, 2))));
    assert!(complete.edges.contains(&(
        group(1, 2, 0),
        ArtifactBindingNode::Artifact(binding(1, 1)),
        ArtifactDependency::Interface,
    )));
    assert_eq!(facts.contexts.len(), 12);
    let (_, restored) = roundtrip(&graph);
    assert_eq!(assert_shared_plan_owners(&restored), 14);
}

#[test]
fn expanded_oracle_detects_dependency_label_changes_after_raw_checksum_is_recomputed() {
    let graph = graph_with_contexts(&[selection(vec![rich_plan(11)])]);
    let expected = expanded_facts(&graph);
    let (bytes, _) = roundtrip(&graph);
    for field in 0..5 {
        let mut changed: EncodedGraph = serde_json::from_slice(&bytes).unwrap();
        let edge = changed
            .witness_edges
            .iter_mut()
            .find(|edge| {
                matches!(
                    &edge.dependency,
                    ArtifactDependency::NativeBinding {
                        record_parent: Some(_),
                        ..
                    }
                )
            })
            .unwrap();
        if let ArtifactDependency::NativeBinding {
            dependent_ordinal,
            generation,
            namespace,
            occurrence,
            record_parent,
        } = &mut edge.dependency
        {
            match field {
                0 => *dependent_ordinal += 1,
                1 => *generation += 1,
                2 => *namespace = "type".into(),
                3 => *occurrence = "other_field".into(),
                4 => *record_parent = Some("OtherRecord".into()),
                _ => unreachable!(),
            }
        }
        sign_raw(&mut changed);
        let restored: RecoveryGraphWire =
            serde_json::from_slice(&serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_ne!(
            expected,
            expanded_facts(&restored),
            "label field {field} disappeared from oracle"
        );
    }
}

proptest::proptest! {
    #![proptest_config({
        let mut config = proptest::test_runner::Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(path)));
        }
        config
    })]
    #[test]
    fn canonical_complete_plans_ignore_insertion_history(
        history in proptest::collection::vec(proptest::num::u8::ANY, 1..33)
    ) {
        let original = rich_plan(11);
        let mut nodes = original.nodes.clone();
        let mut edges = original.dependencies.clone();
        for (step, value) in history.iter().enumerate() {
            let index = step % nodes.len();
            let other = usize::from(*value) % nodes.len();
            nodes.swap(index, other);
            let index = step % edges.len();
            let other = usize::from(*value) % edges.len();
            edges.swap(index, other);
        }
        // Duplicate insertions exercise set construction, while published plan
        // vectors themselves remain sorted and unique as the codec requires.
        nodes.push(nodes[0]);
        edges.push(edges[0].clone());
        let rebuilt = plan(11, nodes, edges);
        let earlier = plan(12, original.nodes.iter().copied(), original.dependencies[..5].iter().cloned());
        let canonical = graph_with_contexts(&[selection(vec![original, Arc::clone(&earlier)])]);
        let reordered = graph_with_contexts(&[selection(vec![earlier, rebuilt])]);
        let (canonical_bytes, canonical_restored) = roundtrip(&canonical);
        let (reordered_bytes, reordered_restored) = roundtrip(&reordered);
        // Body contexts are canonical and identical here; arbitrary permutations
        // of historical body arrays have no checksum-equality promise.
        proptest::prop_assert_eq!(canonical_bytes, reordered_bytes);
        proptest::prop_assert_eq!(expanded_facts(&canonical_restored), expanded_facts(&reordered_restored));
        proptest::prop_assert_eq!(assert_shared_plan_owners(&reordered_restored), 6);
    }
}

fn chain_plan(id: u64, count: usize) -> Arc<ArtifactSelectionPlan> {
    let nodes = (0..count)
        .map(|index| ArtifactBindingNode::Artifact(binding(index as u64 + 100, 77)))
        .collect::<Vec<_>>();
    let edges = nodes
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            (
                pair[0],
                pair[1],
                if index % 3 == 0 {
                    ArtifactDependency::NativeBinding {
                        dependent_ordinal: index as u32,
                        generation: index as u64 + 1,
                        namespace: "value".into(),
                        occurrence: format!("edge_{index}"),
                        record_parent: (index % 2 == 0).then(|| "Owner".into()),
                    }
                } else {
                    ArtifactDependency::Interface
                },
            )
        })
        .collect::<Vec<_>>();
    plan(id, nodes, edges)
}

#[test]
fn seventeen_thousand_node_plan_is_encoded_once_across_many_contexts() {
    let shared = chain_plan(81, 17_000);
    let context = selection(vec![Arc::clone(&shared)]);
    let one = graph_with_contexts(std::slice::from_ref(&context));
    let contexts = vec![context; 64];
    let many = graph_with_contexts(&contexts);
    let one_bytes = {
        let mut encoded = raw(&one);
        sign_raw(&mut encoded);
        serde_json::to_vec(&encoded).unwrap()
    };
    let (many_bytes, restored) = roundtrip(&many);
    let encoded: EncodedGraph = serde_json::from_slice(&many_bytes).unwrap();
    assert_eq!(encoded.selection_plans.len(), 1);
    assert_eq!(encoded.witness_nodes.len(), 17_000);
    assert_eq!(encoded.witness_edges.len(), 16_999);
    assert_eq!(encoded.selection_plans[0].nodes.len(), 17_000);
    assert_eq!(encoded.selection_plans[0].dependencies.len(), 16_999);
    assert!(
        many_bytes.len() < one_bytes.len() + 64 * 12_000,
        "shared contexts repeated witness bytes: one={}, many={}",
        one_bytes.len(),
        many_bytes.len()
    );
    let expanded_plan_bytes = serde_json::to_vec(shared.as_ref()).unwrap().len();
    assert!(many_bytes.len() * 16 < expanded_plan_bytes * 64 * 4);
    assert_eq!(assert_shared_plan_owners(&restored), 64 * 4 - 1);
    eprintln!("schema11 shared-plan contexts=64 plan_rows=1 node_atoms=17000 edge_atoms=16999 single_context_bytes={} repeated_context_bytes={} expanded_plan_bytes={} restored_shared_references=255",one_bytes.len(),many_bytes.len(),expanded_plan_bytes);
}

#[test]
fn distinct_overlapping_growing_plans_share_atoms_without_aliasing_plan_owners() {
    let contexts = (1..=24)
        .map(|width| selection(vec![chain_plan(1000 + width as u64, width * 32)]))
        .collect::<Vec<_>>();
    let graph = graph_with_contexts(&contexts);
    let (bytes, restored) = roundtrip(&graph);
    let encoded: EncodedGraph = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(encoded.selection_plans.len(), 24);
    assert_eq!(encoded.witness_nodes.len(), 768);
    assert_eq!(encoded.witness_edges.len(), 767);
    let plan_nodes: usize = encoded
        .selection_plans
        .iter()
        .map(|plan| plan.nodes.len())
        .sum();
    let plan_edges: usize = encoded
        .selection_plans
        .iter()
        .map(|plan| plan.dependencies.len())
        .sum();
    assert_eq!(plan_nodes, 32 * (24 * 25 / 2));
    assert_eq!(plan_edges, plan_nodes - 24);
    // A dictionary is compact, but distinct complete plans still allocate
    // their own expanded vectors. Charge repeated facts before constructing
    // any Arc; an atom-only budget would incorrectly admit this history.
    let unique_record_bytes =
        (encoded.witness_nodes.len() * std::mem::size_of::<ArtifactBindingNode>()
            + encoded.witness_edges.len() * std::mem::size_of::<BoundEdge>()) as u64;
    let low_limit = unique_record_bytes * 2;
    let error = serde_json::from_slice::<EncodedGraph>(&bytes)
        .unwrap()
        .restore(low_limit)
        .unwrap_err();
    assert!(error.contains("expanded recovery witness facts exceed bound"));
    assert!(error.contains(&format!("limit={low_limit}")));
    let generous = serde_json::from_slice::<EncodedGraph>(&bytes)
        .unwrap()
        .restore(1 << 30)
        .unwrap();
    assert_exact_facts(&graph, &generous);
    assert!(plan_nodes > encoded.witness_nodes.len() * 10);
    let dictionary_bytes = serde_json::to_vec(&encoded.witness_nodes).unwrap().len()
        + serde_json::to_vec(&encoded.witness_edges).unwrap().len();
    assert!(bytes.len() < dictionary_bytes + (plan_nodes + plan_edges) * 12 + 24 * 12_000);
    let expanded_plan_bytes: usize = contexts
        .iter()
        .map(|context| {
            serde_json::to_vec(context.selections[0].as_ref())
                .unwrap()
                .len()
        })
        .sum();
    assert!(
        bytes.len() * 3 < expanded_plan_bytes * 4,
        "overlapping witness atoms were repeated: encoded={}, expanded={expanded_plan_bytes}",
        bytes.len()
    );
    assert_eq!(assert_shared_plan_owners(&restored), 24 * 3);
    let earliest = &restored.nodes[0].graph_selection.selections[0];
    let latest = &restored.nodes[23].graph_selection.selections[0];
    assert_eq!(earliest.nodes.len(), 32);
    assert_eq!(latest.nodes.len(), 768);
    assert!(!Arc::ptr_eq(earliest, latest));
    eprintln!("schema11 distinct-overlap contexts=24 plan_rows=24 node_atoms=768 edge_atoms=767 complete_plan_nodes={plan_nodes} complete_plan_edges={plan_edges} encoded_bytes={} dictionary_bytes={dictionary_bytes} expanded_plan_bytes={expanded_plan_bytes} low_expansion_limit={low_limit} restored_shared_references=72",bytes.len());
}
