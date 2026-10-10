use super::*;

fn seal_raw(raw: &mut EncodedGraph) {
    raw.checksum.clear();
    let mut domain = b"tidepool-recovery-graph-v11\0".to_vec();
    domain.extend_from_slice(&serde_json::to_vec(raw).unwrap());
    raw.checksum = blake3::hash(&domain).to_hex().to_string();
}
fn raw_fixture() -> EncodedGraph {
    let mut wire = super::super::tests::fixture();
    let binding = wire.nodes[0].graph_selection.bindings[0];
    let mut second = binding;
    second.selection = ArtifactSelectionId([2; 32]);
    let mut nodes = vec![
        ArtifactBindingNode::Artifact(binding),
        ArtifactBindingNode::Artifact(second),
    ];
    nodes.sort();
    let dependencies = nodes
        .iter()
        .copied()
        .map(|node| (node, node, ArtifactDependency::Interface))
        .collect();
    wire.nodes[0].graph_selection.selections = vec![Arc::new(ArtifactSelectionPlan {
        selection: ArtifactSelectionId([1; 32]),
        nodes,
        dependencies,
    })];
    let mut raw: EncodedGraph =
        serde_json::from_slice(&serde_json::to_vec(&wire).unwrap()).unwrap();
    seal_raw(&mut raw);
    raw
}
fn refusal(raw: &mut EncodedGraph, limit: u64) -> String {
    seal_raw(raw);
    raw_fixture_bytes_refusal(&serde_json::to_vec(raw).unwrap(), limit)
}
fn raw_fixture_bytes_refusal(bytes: &[u8], limit: u64) -> String {
    serde_json::from_slice::<EncodedGraph>(bytes)
        .unwrap()
        .restore(limit)
        .unwrap_err()
}

#[test]
fn malformed_dictionary_and_references_refuse_after_matching_raw_checksum() {
    let cases: Vec<(fn(&mut EncodedGraph), &str)> = vec![
        (
            |raw| raw.witness_nodes.push(raw.witness_nodes[0]),
            "sorted and unique",
        ),
        (
            |raw| raw.witness_edges.push(raw.witness_edges[0].clone()),
            "sorted and unique",
        ),
        (
            |raw| {
                raw.selection_plans.push(PlanRecord {
                    selection: raw.selection_plans[0].selection,
                    nodes: vec![],
                    dependencies: vec![],
                })
            },
            "sorted and unique",
        ),
        (
            |raw| raw.witness_edges[0].target = u32::MAX,
            "endpoint index",
        ),
        (
            |raw| raw.selection_plans[0].nodes = vec![u32::MAX],
            "node index",
        ),
        (
            |raw| raw.selection_plans[0].dependencies = vec![u32::MAX],
            "edge index",
        ),
        (
            |raw| raw.selection_plans[0].nodes.push(0),
            "sorted and unique",
        ),
        (
            |raw| raw.selection_plans[0].dependencies.push(0),
            "sorted and unique",
        ),
        (
            |raw| {
                raw.nodes[0]
                    .graph_selection
                    .selections
                    .push(ArtifactSelectionId([1; 32]))
            },
            "duplicate selection witness reference",
        ),
        (
            |raw| raw.nodes[0].graph_selection.selections[0] = ArtifactSelectionId([99; 32]),
            "missing selection witness reference",
        ),
        (
            |raw| raw.nodes[0].graph_selection.selections.clear(),
            "unreferenced selection witness",
        ),
        (
            |raw| raw.selection_plans[0].dependencies.clear(),
            "unused witness dictionary",
        ),
    ];
    let mut cases = cases;
    cases.extend([
        (
            (|raw: &mut EncodedGraph| raw.witness_nodes.reverse()) as fn(&mut EncodedGraph),
            "sorted and unique",
        ),
        (
            (|raw: &mut EncodedGraph| raw.witness_edges.reverse()) as fn(&mut EncodedGraph),
            "sorted and unique",
        ),
        (
            (|raw: &mut EncodedGraph| raw.selection_plans[0].nodes.reverse())
                as fn(&mut EncodedGraph),
            "sorted and unique",
        ),
        (
            (|raw: &mut EncodedGraph| raw.selection_plans[0].dependencies.reverse())
                as fn(&mut EncodedGraph),
            "sorted and unique",
        ),
        (
            (|raw: &mut EncodedGraph| {
                let mut extra = raw.witness_nodes[0];
                if let ArtifactBindingNode::Artifact(ref mut binding) = extra {
                    binding.selection = ArtifactSelectionId([254; 32]);
                }
                raw.witness_nodes.push(extra);
                raw.witness_nodes.sort();
            }) as fn(&mut EncodedGraph),
            "unused witness dictionary",
        ),
    ]);
    for (mutate, expected) in cases {
        let mut raw = raw_fixture();
        mutate(&mut raw);
        assert!(
            refusal(&mut raw, MAX_EXPANDED_WITNESS_BYTES).contains(expected),
            "expected {expected}"
        );
    }
    // Refusal precedence proves that unused/unknown/duplicate references are
    // inspected before the expansion budget and allocation of complete plans.
    for kind in 0..3 {
        let mut raw = raw_fixture();
        let expected = match kind {
            0 => {
                raw.nodes[0].graph_selection.selections.clear();
                "unreferenced"
            }
            1 => {
                raw.nodes[0].graph_selection.selections[0] = ArtifactSelectionId([99; 32]);
                "missing"
            }
            _ => {
                raw.nodes[0]
                    .graph_selection
                    .selections
                    .push(ArtifactSelectionId([1; 32]));
                "duplicate"
            }
        };
        assert!(refusal(&mut raw, 0).contains(expected));
    }
    let error = refusal(&mut raw_fixture(), 0);
    assert!(error.contains("expanded recovery witness facts exceed bound"));
    assert!(error.contains("bytes=") && error.contains("limit=0"));
}

#[test]
fn writer_refuses_reordered_complete_facts_and_conflicting_late_claim() {
    let mut wire = raw_fixture().restore(MAX_EXPANDED_WITNESS_BYTES).unwrap();
    let plan = Arc::make_mut(&mut wire.nodes[0].graph_selection.selections[0]);
    plan.nodes.reverse();
    let recovered = ExactArtifactSelection::from_recovered_graph_selection(
        wire.nodes[0].graph_selection.clone(),
    );
    wire.nodes[0].graph_selection = recovered.graph_selection().clone();
    assert!(serde_json::to_vec(&wire)
        .unwrap_err()
        .to_string()
        .contains("sorted and unique"));
    wire.nodes[0].graph_selection.selections = raw_fixture()
        .restore(MAX_EXPANDED_WITNESS_BYTES)
        .unwrap()
        .nodes[0]
        .graph_selection
        .selections
        .clone();
    let mut other = wire.nodes[0].graph_selection.selections[0].as_ref().clone();
    other.dependencies.clear();
    wire.nodes[1].graph_selection.selections = vec![Arc::new(other)];
    assert!(serde_json::to_vec(&wire)
        .unwrap_err()
        .to_string()
        .contains("differing complete witness facts"));
}

#[test]
fn raw_body_reorder_preserves_revision_and_requires_candidate_reseal() {
    let wire = super::super::tests::fixture();
    let mut raw: EncodedGraph =
        serde_json::from_slice(&serde_json::to_vec(&wire).unwrap()).unwrap();
    raw.nodes.reverse();
    raw.artifacts.reverse();
    raw.public_surfaces.reverse();
    raw.artifact_bindings.reverse();
    seal_raw(&mut raw);
    let revision = raw.checksum.clone();
    let bytes = serde_json::to_vec(&raw).unwrap();
    let graph: RecoveryGraph = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(graph.checksum(), revision);
    assert!(serde_json::to_vec(&graph).is_err());
    let mut candidate = graph.candidate();
    candidate.set_high_water(Generation(3)).unwrap();
    let successor = candidate.seal().unwrap();
    let decoded: RecoveryGraph =
        serde_json::from_slice(&serde_json::to_vec(&successor).unwrap()).unwrap();
    assert_eq!(decoded, successor);
    assert_eq!(graph.checksum(), revision);
}

#[test]
fn schema_header_refuses_version_only_old_and_future_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.json");
    for version in [u64::from(VERSION) - 1, u64::from(VERSION) + 1] {
        let bytes = format!("{{\"version\":{version}}}").into_bytes();
        fs::write(&path, &bytes).unwrap();
        let error = read_v2(&path, dir.path()).err().unwrap();
        assert_eq!(
            error.kind,
            RecoveryErrorKind::Format(if version < u64::from(VERSION) {
                RecoveryRefusal::UnsupportedOldFormat { version }
            } else {
                RecoveryRefusal::UnsupportedFutureFormat { version }
            })
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn bounded_reader_distinguishes_absence_from_open_refusal_and_reports_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.json");
    assert!(read_v2(&missing, dir.path()).unwrap().is_none());
    let cycle = dir.path().join("cycle.json");
    std::os::unix::fs::symlink("cycle.json", &cycle).unwrap();
    assert!(read_v2(&cycle, dir.path()).is_err());
    let huge = dir.path().join("huge.json");
    let bytes = MAX_MANIFEST_BYTES as u64 + 137;
    fs::File::create(&huge).unwrap().set_len(bytes).unwrap();
    let error = read_v2(&huge, dir.path()).err().unwrap();
    assert_eq!(error.path, Some(huge));
    assert!(error.detail.contains(&format!("observed_bytes={bytes}")));
    assert!(error
        .detail
        .contains(&format!("limit_bytes={MAX_MANIFEST_BYTES}")));
}

#[test]
fn raw_duplicate_object_fields_and_checksum_tamper_refuse() {
    let raw = raw_fixture();
    let bytes = serde_json::to_string(&raw).unwrap();
    let duplicate = bytes.replacen("{", &format!("{{\"version\":{},", VERSION), 1);
    assert!(serde_json::from_str::<RecoveryGraphWire>(&duplicate)
        .unwrap_err()
        .to_string()
        .contains("duplicate field"));
    let fixture = super::super::tests::fixture();
    let valid = serde_json::to_string(&fixture).unwrap();
    let duplicate = valid.replacen(
        "{",
        &format!("{{\"source_session\":{},", fixture.source_session),
        1,
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("duplicate.json");
    let error = read_v2_bytes(
        &path,
        dir.path(),
        duplicate.as_bytes(),
        RecoveryReadPurpose::Metadata,
    )
    .err()
    .unwrap();
    assert!(error.detail.contains("duplicate field"));
    let mut bad = raw;
    bad.nodes.reverse();
    assert!(
        serde_json::from_slice::<RecoveryGraphWire>(&serde_json::to_vec(&bad).unwrap())
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch")
    );
}
