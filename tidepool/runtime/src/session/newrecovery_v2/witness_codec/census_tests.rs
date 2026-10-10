//! Explicit external evidence adapter; production still refuses schema 10.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyGraph {
    version: u32,
    public_schema: String,
    source_session: u64,
    lineage: u64,
    high_water: u64,
    public_surfaces: Vec<RecoveryPublicSurface>,
    nodes: Vec<RecoveryNode>,
    artifacts: Vec<RecoveryArtifactClosure>,
    artifact_bindings: Vec<ArtifactBinding>,
    artifact_dependencies: Vec<RecoveryArtifactDependency>,
    checksum: String,
}
fn all_selections(wire: &RecoveryGraphWire) -> Vec<&ArtifactGraphSelection> {
    let mut selections = vec![];
    for node in &wire.nodes {
        selections.push(&node.graph_selection);
        for role in &node.compiler_roles {
            if let CompilerInputRole::PublishedSourceOriginal { selection, .. } = role {
                selections.push(selection.graph_selection());
            }
        }
    }
    for surface in &wire.public_surfaces {
        selections.push(&surface.compiler_context.graph_selection);
        for role in &surface.compiler_context.compiler_roles {
            if let CompilerInputRole::PublishedSourceOriginal { selection, .. } = role {
                selections.push(selection.graph_selection());
            }
        }
    }
    selections
}

#[test]
#[ignore = "requires the explicitly retained frozen060 full-workspace manifest"]
fn retained_full_workspace_manifest_codec_census() {
    let input = std::env::var_os("TIDEPOOL_RECOVERY_CENSUS_INPUT")
        .expect("explicit retained census input is required");
    let original = fs::read(&input).unwrap();
    assert_eq!(original.len(), 61_090_813);
    let original_sha = format!("{:x}", Sha256::digest(&original));
    assert_eq!(
        original_sha,
        "5128ee586e71493315a3496e1434b352ce208730b6dad59ae4c8f5bb0ecf5f2a"
    );
    let mut legacy: LegacyGraph = serde_json::from_slice(&original).unwrap();
    assert_eq!(legacy.version, 10);
    assert_eq!(legacy.public_schema, "paired-public-v10");
    let legacy_checksum = std::mem::take(&mut legacy.checksum);
    let mut domain = b"tidepool-recovery-graph-v10\0".to_vec();
    domain.extend_from_slice(&serde_json::to_vec(&legacy).unwrap());
    assert_eq!(blake3::hash(&domain).to_hex().as_str(), legacy_checksum);
    // This test adapter carries exactly the authenticated retained facts, not a
    // guessed migration or authority grant. Actual native admission is exercised
    // independently by the maintained recovery consumer controls.
    let mut wire = RecoveryGraphWire {
        version: VERSION,
        public_schema: PAIRED_PUBLIC_SCHEMA.into(),
        source_session: legacy.source_session,
        lineage: legacy.lineage,
        high_water: Generation(legacy.high_water),
        public_surfaces: legacy.public_surfaces,
        nodes: legacy.nodes,
        artifacts: legacy.artifacts,
        artifact_bindings: legacy.artifact_bindings,
        artifact_dependencies: legacy.artifact_dependencies,
        checksum: String::new(),
    };
    wire.checksum = checksum(&wire).unwrap();
    let encoded = serde_json::to_vec(&wire).unwrap();
    assert!(encoded.len() < MAX_MANIFEST_BYTES);
    let raw: EncodedGraph = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(raw.selection_plans.len(), 1);
    assert_eq!(raw.witness_nodes.len(), 17_885);
    assert_eq!(raw.witness_edges.len(), 38_531);
    let census = (
        raw.selection_plans.len(),
        raw.witness_nodes.len(),
        raw.witness_edges.len(),
    );
    let restored: RecoveryGraphWire = serde_json::from_slice(&encoded).unwrap();
    let before = all_selections(&wire);
    let after = all_selections(&restored);
    assert_eq!(before.len(), 2);
    assert_eq!(after.len(), 2);
    for (original, recovered) in before.into_iter().zip(after.iter()) {
        assert_eq!(
            original, *recovered,
            "every complete scoped node/edge and selected role fact survives the production codec"
        );
    }
    assert_eq!(wire.public_surfaces, restored.public_surfaces);
    assert_eq!(wire.nodes, restored.nodes);
    assert_eq!(wire.artifacts, restored.artifacts);
    assert_eq!(wire.artifact_bindings, restored.artifact_bindings);
    assert_eq!(wire.artifact_dependencies, restored.artifact_dependencies);
    assert!(Arc::ptr_eq(
        &after[0].selections[0],
        &after[1].selections[0]
    ));
    let encoded_sha = format!("{:x}", Sha256::digest(&encoded));
    let report = serde_json::json!({
        "kind":"authenticated retained schema10 facts adapted through production schema11 codec; not fresh publication",
        "input_path":PathBuf::from(input),"legacy_bytes":original.len(),"legacy_sha256":original_sha,
        "legacy_checksum":legacy_checksum,"schema11_bytes":encoded.len(),"schema11_sha256":encoded_sha,
        "schema11_checksum":restored.checksum,"plans":census.0,"node_atoms":census.1,"edge_atoms":census.2,
        "outer_and_nested_complete_facts_equal":true,"restored_plan_arc_shared":true,
        "distinct_plan_growth":"qualified separately by generated overlapping-history controls",
    });
    let evidence = PathBuf::from(
        std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").expect("runner-owned evidence root"),
    );
    fs::write(evidence.join("retained060-schema11.json"), &encoded).unwrap();
    fs::write(
        evidence.join("retained060-schema11-census.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("{report}");
}
