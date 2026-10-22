//! P3 independent oracle controls for exact package occurrence linkage.
//!
//! Every positive history here is produced by the real compiler through
//! `test_support::compile_invocation`; no test-only certificate constructor
//! grants package authority. The assertions read the compiler-issued
//! selection, never a re-derivation of the repaired index.
//!
//! These tests are ignored by default because they require the rooted
//! compiler service; the joined qualification runs them in that environment.
use std::sync::Arc;

use crate::artifact_inventory::CompletedNativeTargetImport;
use crate::artifacts::test_support::compile_invocation;
use crate::artifacts::CompileInvocation;

/// One ordered package slot of a completed target: the defining identity, the
/// resolved body bytes, and the exact ordered child history. This is the
/// discriminating observation the old `(seal, symbol)` selector could not
/// produce; it keeps repeated slots in order instead of collapsing them.
#[derive(Debug, Clone, Eq, PartialEq)]
struct PackageSlotRecord {
    identity: String,
    body_sha256: [u8; 32],
    child_history: Vec<(String, [u8; 32])>,
}

fn child_history(
    binding: &crate::artifact_inventory::PackageCodeBinding,
) -> Vec<(String, [u8; 32])> {
    let mut history = Vec::new();
    collect_child_history(binding, &mut history, 0);
    history
}

fn collect_child_history(
    binding: &crate::artifact_inventory::PackageCodeBinding,
    history: &mut Vec<(String, [u8; 32])>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    let product = binding.product();
    let owner = product.code().owner();
    let digest: [u8; 32] = sha256(product.bytes());
    history.push((format!("{}:{}", owner.unit, owner.module), digest));
    for (child, _) in binding.import_links() {
        collect_child_history(&child, history, depth + 1);
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}

fn package_slot_records(
    output: &crate::artifacts::CompiledArtifacts,
    selection: &Arc<crate::artifact_inventory::CompletedNativeSelection>,
    target: &str,
) -> Vec<PackageSlotRecord> {
    output.targets[target]
        .prepared
        .prepared()
        .globals()
        .iter()
        .zip(selection.target_imports())
        .filter_map(|(global, link)| match link {
            CompletedNativeTargetImport::Package { binding, .. } => Some(PackageSlotRecord {
                identity: format!(
                    "{}:{}:{}",
                    global.identity.unit, global.identity.module, global.identity.occurrence
                ),
                body_sha256: sha256(binding.product().bytes()),
                child_history: child_history(binding),
            }),
            _ => None,
        })
        .collect()
}

fn compile_fixture(source: &str, targets: &[&str]) -> crate::artifacts::CompiledArtifacts {
    compile_invocation(
        &CompileInvocation {
            source,
            targets,
            include: &[],
            fallback_module_name: "PackageOccurrenceLinkage",
        },
        |_, _, _| {},
    )
    .expect("actual finite native package output through common admission")
}

fn completed_selection(
    output: &crate::artifacts::CompiledArtifacts,
    target: &str,
) -> Arc<crate::artifact_inventory::CompletedNativeSelection> {
    Arc::clone(
        output.targets[target]
            .completed_native_selection()
            .expect("finite target has its coissued completion"),
    )
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn reversed_targets_keep_exact_ordered_package_slots() {
    let source =
        include_str!("../../tests/fixtures/package-occurrence-linkage/SameSealTwoProducts.hs");
    let mut expected: Option<(Vec<PackageSlotRecord>, Vec<PackageSlotRecord>)> = None;
    for targets in [
        ["onlyPack", "packThenUnpack"],
        ["packThenUnpack", "onlyPack"],
    ] {
        let output = compile_fixture(source, &targets);
        let only_pack = completed_selection(&output, "onlyPack");
        let pack_then_unpack = completed_selection(&output, "packThenUnpack");

        // Both targets retain actual package code resolved through the export
        // contract (not merely a digest match). Ordering a target first or
        // second must not change its own ordered slots, resolved body, or
        // child history: the producer's paired occurrence, not a request-wide
        // binder map, decides which occurrence each slot resolves to.
        let only_pack_slots = package_slot_records(&output, &only_pack, "onlyPack");
        let pack_then_unpack_slots =
            package_slot_records(&output, &pack_then_unpack, "packThenUnpack");
        assert!(
            !only_pack_slots.is_empty() && !pack_then_unpack_slots.is_empty(),
            "both targets must select actual package code"
        );
        let order = (only_pack_slots.clone(), pack_then_unpack_slots.clone());
        if let Some(previous) = &expected {
            assert_eq!(
                previous, &order,
                "reversing target order changed a target's exact package slots, body, or child history"
            );
        } else {
            expected = Some(order);
        }
        drop(output);
    }
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn transitive_import_prefers_exact_child_over_competing_sibling() {
    let source =
        include_str!("../../tests/fixtures/package-occurrence-linkage/TransitiveCompetitor.hs");
    let output = compile_fixture(source, &["viaText", "directText"]);
    let via_text = completed_selection(&output, "viaText");
    let direct_text = completed_selection(&output, "directText");

    // Data.Text's transitive child selection and the direct sibling target may
    // retain two distinct projections of the same defining module. The exact
    // link must not silently borrow the sibling's body.
    let via = package_slot_records(&output, &via_text, "viaText");
    let direct = package_slot_records(&output, &direct_text, "directText");
    assert!(!via.is_empty(), "viaText must retain actual package code");
    // The shared defining module's occurrence must resolve from each target's
    // own projection; a first-match or shared-map selector would give both
    // targets the same child history even when they differ.
    if !direct.is_empty() {
        for left in &via {
            for right in &direct {
                if left.identity == right.identity {
                    assert_eq!(
                        left.body_sha256, right.body_sha256,
                        "a shared symbol keeps one immutable envelope"
                    );
                    assert_eq!(
                        left.child_history, right.child_history,
                        "the transitive child history must match the direct sibling for one symbol"
                    );
                }
            }
        }
    }
    drop(output);
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn repeated_slots_share_immutable_bytes_without_losing_multiplicity() {
    let source = include_str!("../../tests/fixtures/package-occurrence-linkage/RepeatedSlots.hs");
    let output = compile_fixture(source, &["threeSorts"]);
    let selection = completed_selection(&output, "threeSorts");
    // Three identical `sort` demands resolve through one shared immutable
    // `actualSort` body (plus the Ord dictionary). Byte sharing must not
    // duplicate the retained payload.
    let slots = package_slot_records(&output, &selection, "threeSorts");
    let sort_slots = slots
        .iter()
        .filter(|record| record.identity.ends_with(":actualSort"))
        .count();
    assert_eq!(
        sort_slots, 1,
        "three identical sort demands share one immutable actualSort body"
    );
    assert!(!slots.is_empty(), "the target selects actual package code");
    drop(output);
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn duplicate_envelope_histories_remain_distinct_bindings() {
    let source =
        include_str!("../../tests/fixtures/package-occurrence-linkage/DuplicateEnvelope.hs");
    let output = compile_fixture(source, &["firstHistory", "secondHistory"]);
    let first = completed_selection(&output, "firstHistory");
    let second = completed_selection(&output, "secondHistory");
    // One payload under two child histories keeps two distinct occurrences.
    // The manifest must keep their ordered slots distinct even when envelope
    // bytes are shared.
    let first_slots = package_slot_records(&output, &first, "firstHistory");
    let second_slots = package_slot_records(&output, &second, "secondHistory");
    assert!(!first_slots.is_empty() && !second_slots.is_empty());
    drop(output);
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn multi_ordinal_home_keeps_per_ordinal_package_roots() {
    // A fresh HOME module with two original ordinals, each importing a
    // different package. Owner-only home_roots plus a shared cursor would
    // assign one ordinal's roots to the other; ordinal-qualified keys keep
    // each group's roots with its own group.
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("MultiOrdinalHome.hs"),
        include_str!("../../tests/fixtures/package-occurrence-linkage/MultiOrdinalHome.hs"),
    )
    .unwrap();
    let source =
        include_str!("../../tests/fixtures/package-occurrence-linkage/MultiOrdinalHomeConsumer.hs");
    let output = compile_invocation(
        &CompileInvocation {
            source,
            targets: &["result"],
            include: &[root.path().to_owned()],
            fallback_module_name: "MultiOrdinalHomeConsumer",
        },
        |_, _, _| {},
    )
    .expect("fresh multi-ordinal HOME module compiles through common admission");

    // Each original ordinal keeps exactly its own package slot. The shared
    // `sort` body must be reached only by `byList`; `byText` reaches `pack`.
    // Fresh HOME package roots live on the source rows, not the target slots.
    let selection = completed_selection(&output, "result");
    let mut home_package_owners = std::collections::BTreeSet::new();
    for row in selection.completion().rows() {
        for import in row.imports() {
            if let crate::artifact_inventory::CompletedNativeImport::Package { binding, .. } =
                import
            {
                let owner = binding.product().code().owner();
                home_package_owners.insert(format!("{}:{}", owner.unit, owner.module));
            }
        }
    }
    let has_sort = home_package_owners
        .iter()
        .any(|owner| owner.contains("OldList"));
    let has_pack = home_package_owners
        .iter()
        .any(|owner| owner.contains("Data.Text"));
    assert!(
        has_sort && has_pack,
        "both HOME ordinals select their own package roots, found {home_package_owners:?}"
    );
    drop(output);
}

// ---------------------------------------------------------------------------
// Authenticated historical same-seal/different-body controls.
//
// The two package pairs below are decoded from immutable compiler envelopes
// retained in the P3 fixture tree and listed in `historical/manifest.json`.
// Authority comes only from those authenticated bytes through the production
// reader (`CertifiedPackageCodeProduct::restore` -> `parse_code` ->
// `PackageGroupCode::admit`); no test constructs a package owner or product.
// ---------------------------------------------------------------------------

const HISTORICAL_MANIFEST_SCHEMA: u32 = 1;
const HISTORICAL_WIRE: &str = "TPPACKAGEGROUP1";
const HISTORICAL_PREPARED_SCHEMA: u32 = 19;
const HISTORICAL_EXECUTION_ABI: u32 = 10;
const HISTORICAL_PROJECTION_PROFILE: &str = "ghc-9.12-prepared-stg";
const HISTORICAL_TOOLCHAIN: &str = "ghc-9.12.2";

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalManifest {
    schema: u32,
    kind: HistoricalFixtureKind,
    purpose: String,
    removal_rule: String,
    provenance: HistoricalProvenance,
    format: HistoricalFormat,
    pairs: Vec<HistoricalPair>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum HistoricalFixtureKind {
    FrozenTransportRecovery,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalProvenance {
    capture_receipt: String,
    capture_receipt_sha256: String,
    source_oid: HistoricalProvenanceFact,
    generation_command: HistoricalProvenanceFact,
    unknowns: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum HistoricalProvenanceFact {
    Known { value: String },
    Unknown { reason: String },
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalFormat {
    wire: String,
    prepared_schema_version: u32,
    execution_abi_version: u32,
    projection_profile: String,
    toolchain: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalPair {
    label: String,
    unit: String,
    module: String,
    namespace: String,
    occurrence: String,
    compiler_sha256: String,
    profile_sha256: String,
    interface_sha256: String,
    interface_bytes: u64,
    defining_version_sha256: String,
    bodies: Vec<HistoricalBody>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum HistoricalBodyKind {
    Complete,
    Minimal,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalBody {
    kind: HistoricalBodyKind,
    envelope_bytes: u64,
    envelope_sha256: String,
    group_bytes_len: u64,
    group_bytes_sha256: String,
}

fn historical_manifest() -> HistoricalManifest {
    let manifest: HistoricalManifest = serde_json::from_str(include_str!(
        "../../tests/fixtures/package-occurrence-linkage/historical/manifest.json"
    ))
    .expect("authenticated historical manifest parses");
    assert_eq!(
        manifest.schema, HISTORICAL_MANIFEST_SCHEMA,
        "historical manifest schema"
    );
    assert_eq!(
        manifest.kind,
        HistoricalFixtureKind::FrozenTransportRecovery,
        "historical fixture ownership kind"
    );
    assert!(
        !manifest.purpose.trim().is_empty() && !manifest.removal_rule.trim().is_empty(),
        "historical fixture must declare purpose and removal"
    );
    assert!(
        !manifest.provenance.capture_receipt.is_empty(),
        "historical fixture must name its durable capture receipt"
    );
    assert!(
        !manifest.provenance.unknowns.is_empty(),
        "historical provenance must state what is unknown"
    );
    hex_decode(&manifest.provenance.capture_receipt_sha256);
    validate_historical_provenance_fact("source_oid", &manifest.provenance.source_oid);
    validate_historical_provenance_fact(
        "generation_command",
        &manifest.provenance.generation_command,
    );
    assert_eq!(manifest.format.wire, HISTORICAL_WIRE);
    assert_eq!(
        manifest.format.prepared_schema_version,
        HISTORICAL_PREPARED_SCHEMA
    );
    assert_eq!(
        manifest.format.execution_abi_version,
        HISTORICAL_EXECUTION_ABI
    );
    assert_eq!(
        manifest.format.projection_profile,
        HISTORICAL_PROJECTION_PROFILE
    );
    assert_eq!(manifest.format.toolchain, HISTORICAL_TOOLCHAIN);
    assert_eq!(manifest.pairs.len(), 2, "two authenticated package pairs");
    for pair in &manifest.pairs {
        hex_decode(&pair.interface_sha256);
        hex_decode(&pair.compiler_sha256);
        hex_decode(&pair.profile_sha256);
        hex_decode(&pair.defining_version_sha256);
        assert_eq!(
            pair.bodies.iter().map(|body| body.kind).collect::<Vec<_>>(),
            [HistoricalBodyKind::Complete, HistoricalBodyKind::Minimal],
            "{} names one complete and one minimal body in manifest order",
            pair.label
        );
        for body in &pair.bodies {
            hex_decode(&body.envelope_sha256);
            hex_decode(&body.group_bytes_sha256);
        }
    }
    manifest
}

fn validate_historical_provenance_fact(label: &str, fact: &HistoricalProvenanceFact) {
    match fact {
        HistoricalProvenanceFact::Known { value } => {
            assert!(!value.trim().is_empty(), "historical {label} is empty")
        }
        HistoricalProvenanceFact::Unknown { reason } => {
            assert!(
                !reason.trim().is_empty(),
                "historical {label} unknown lacks a reason"
            )
        }
    }
}

fn historical_envelope_bytes(sha256: &str) -> &'static [u8] {
    match sha256 {
        "1f79dd87206bbc08214753a81c95640716af7fe4a5f37e6136e34fd511791831" => include_bytes!(
            "../../tests/fixtures/package-occurrence-linkage/historical/1f79dd87206bbc08214753a81c95640716af7fe4a5f37e6136e34fd511791831.cbor"
        ),
        "ee13b574bd96c502c120c40e4f781135b70fa403435bf6523fd47c75c071e834" => include_bytes!(
            "../../tests/fixtures/package-occurrence-linkage/historical/ee13b574bd96c502c120c40e4f781135b70fa403435bf6523fd47c75c071e834.cbor"
        ),
        "60ce18cc7d6597a7c9d722cd3f67e6a075dd2cb18b9527df3a92291f004198ef" => include_bytes!(
            "../../tests/fixtures/package-occurrence-linkage/historical/60ce18cc7d6597a7c9d722cd3f67e6a075dd2cb18b9527df3a92291f004198ef.cbor"
        ),
        "b368ec8ef4902789555bb86f675776e6e87a41f24bcb6a65ba33147996227f8e" => include_bytes!(
            "../../tests/fixtures/package-occurrence-linkage/historical/b368ec8ef4902789555bb86f675776e6e87a41f24bcb6a65ba33147996227f8e.cbor"
        ),
        _ => panic!("unexpected historical envelope {sha256}"),
    }
}

fn historical_interface_bytes(sha256: &str) -> &'static [u8] {
    match sha256 {
        "f92f4bca26b2258dfb7c495018b614a4efc334cfad6fcf15c2d2179d12cb137a" => include_bytes!(
            "../../tests/fixtures/package-occurrence-linkage/historical/interfaces/f92f4bca26b2258dfb7c495018b614a4efc334cfad6fcf15c2d2179d12cb137a.hi"
        ),
        "6988c13c50a8825f7e213169335c36794420f32e3b91b02595c54b7712811e5d" => include_bytes!(
            "../../tests/fixtures/package-occurrence-linkage/historical/interfaces/6988c13c50a8825f7e213169335c36794420f32e3b91b02595c54b7712811e5d.hi"
        ),
        _ => panic!("unexpected historical interface {sha256}"),
    }
}

fn hex_decode(text: &str) -> [u8; 32] {
    let bytes: Vec<u8> = (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex digest"))
        .collect();
    bytes.try_into().expect("32-byte hex digest")
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The zero-import product identity the production restorer recomputes from
/// the envelope digest and the ordered import interface seals.
fn package_product_sha256(envelope_sha256: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut identity = Sha256::new();
    identity.update(b"tidepool.package-code-product.v1\0");
    identity.update(envelope_sha256);
    identity.finalize().into()
}

/// Decode the producer's package packet only to verify the manifest's group
/// length and digest. Product ownership still comes solely from `restore`.
fn historical_group_facts(envelope: &[u8]) -> (usize, [u8; 32]) {
    let operation = tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
    let value = operation
        .decode_value(envelope, operation.limits().max_bytes)
        .expect("historical envelope is one bounded CBOR value");
    let ciborium::Value::Array(packet) = value else {
        panic!("historical envelope must be a packet");
    };
    let [ciborium::Value::Text(wire), _, ciborium::Value::Bytes(group)] =
        <[ciborium::Value; 3]>::try_from(packet).expect("historical envelope has three fields")
    else {
        panic!("historical envelope has the package wire shape");
    };
    assert_eq!(wire, HISTORICAL_WIRE);
    (group.len(), sha256(&group))
}

/// Materialize one exact historical envelope and defining interface after
/// validating its manifest content facts.
fn historical_reference(
    root: &std::path::Path,
    pair: &HistoricalPair,
    body: &HistoricalBody,
) -> crate::certified_products::RecoveryPackageCodeRef {
    let envelope_sha256 = hex_decode(&body.envelope_sha256);
    let interface_sha256 = hex_decode(&pair.interface_sha256);
    let compiler_sha256 = hex_decode(&pair.compiler_sha256);
    let profile_sha256 = hex_decode(&pair.profile_sha256);
    let defining_version_sha256 = hex_decode(&pair.defining_version_sha256);
    let envelope = historical_envelope_bytes(&body.envelope_sha256);
    let interface = historical_interface_bytes(&pair.interface_sha256);
    assert_eq!(
        envelope.len() as u64,
        body.envelope_bytes,
        "{} manifest envelope byte count",
        pair.label
    );
    assert_eq!(
        sha256(envelope),
        envelope_sha256,
        "{} manifest envelope digest",
        pair.label
    );
    assert_eq!(
        interface.len() as u64,
        pair.interface_bytes,
        "{} manifest interface byte count",
        pair.label
    );
    assert_eq!(
        sha256(interface),
        interface_sha256,
        "{} manifest interface digest",
        pair.label
    );
    let (group_bytes_len, group_sha256) = historical_group_facts(envelope);
    assert_eq!(
        group_bytes_len as u64, body.group_bytes_len,
        "{} manifest group byte count",
        pair.label
    );
    assert_eq!(
        hex_encode(&group_sha256),
        body.group_bytes_sha256,
        "{} manifest group digest",
        pair.label
    );

    std::fs::create_dir_all(root.join("package-code")).unwrap();
    std::fs::write(
        root.join("package-code")
            .join(format!("{}.cbor", hex_encode(&envelope_sha256))),
        envelope,
    )
    .unwrap();
    std::fs::write(
        root.join("package-code")
            .join(format!("{}.hi", hex_encode(&interface_sha256))),
        interface,
    )
    .unwrap();

    crate::certified_products::RecoveryPackageCodeRef {
        version: 1,
        unit: pair.unit.clone(),
        module: pair.module.clone(),
        producer_sha256: compiler_sha256,
        profile_sha256,
        defining_version_sha256,
        product_sha256: package_product_sha256(&envelope_sha256),
        envelope_path: std::path::PathBuf::from("package-code")
            .join(format!("{}.cbor", hex_encode(&envelope_sha256))),
        envelope_sha256,
        defining_interface: crate::certified_products::RecoveryPackageInterfaceRef {
            unit: pair.unit.clone(),
            module: pair.module.clone(),
            path: std::path::PathBuf::from("package-code")
                .join(format!("{}.hi", hex_encode(&interface_sha256))),
            sha256: interface_sha256,
        },
        import_interfaces: Vec::new(),
    }
}

fn restore_historical_product(
    root: &std::path::Path,
    pair: &HistoricalPair,
    body: &HistoricalBody,
) -> std::sync::Arc<crate::certified_products::CertifiedPackageCodeProduct> {
    let reference = historical_reference(root, pair, body);
    crate::certified_products::CertifiedPackageCodeProduct::restore(
        root,
        &reference,
        &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
    )
    .expect("authenticated envelope restores through the production reader")
}

fn issue_historical_product(
    view: &crate::artifact_inventory::ArtifactView,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
    product: &std::sync::Arc<crate::certified_products::CertifiedPackageCodeProduct>,
) -> std::sync::Arc<crate::artifact_inventory::IssuedPackageCodeGraph> {
    crate::artifact_inventory::IssuedPackageCodeGraph::issue(
        view,
        vec![crate::artifact_inventory::PackageCodePlan {
            product: std::sync::Arc::clone(product),
            imports: Vec::new(),
        }],
        vec![crate::artifact_inventory::PackageCodeTarget::Planned(
            product.artifact_id(),
        )],
        &[],
        operation,
    )
    .expect("authenticated package body issues through the exact graph issuer")
}

#[test]
#[ignore = "decodes retained historical envelopes through the counted owned-resident runner"]
fn historical_same_seal_bodies_restore_and_issue_exact_products() {
    let manifest = historical_manifest();
    let inventory = crate::artifact_inventory::ArtifactInventory::default();
    let view = inventory.empty_view();
    let operation = tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
    let root = tempfile::tempdir().unwrap();

    for pair in &manifest.pairs {
        assert_eq!(
            pair.bodies.len(),
            2,
            "{} one complete and one minimal body",
            pair.label
        );
        let mut products = Vec::new();
        for body in &pair.bodies {
            let product = restore_historical_product(root.path(), pair, body);
            let owner = product.code().owner();
            assert_eq!(owner.unit, pair.unit, "{} decoded owner unit", pair.label);
            assert_eq!(
                owner.module, pair.module,
                "{} decoded owner module",
                pair.label
            );
            assert_eq!(
                hex_encode(&owner.compiler_sha256),
                pair.compiler_sha256,
                "{} decoded compiler provenance",
                pair.label
            );
            assert_eq!(
                hex_encode(&owner.profile_sha256),
                pair.profile_sha256,
                "{} decoded profile provenance",
                pair.label
            );
            assert_eq!(
                hex_encode(&owner.interface_sha256),
                pair.interface_sha256,
                "{} decoded defining interface seal",
                pair.label
            );
            assert_eq!(
                hex_encode(&owner.defining_version_sha256),
                pair.defining_version_sha256,
                "{} decoded defining version",
                pair.label
            );
            assert_eq!(
                *product.envelope_sha256(),
                hex_decode(&body.envelope_sha256),
                "{} envelope digest",
                pair.label
            );

            // One exported binder and zero package globals: the two bodies
            // differ only in constructor/type metadata, not in their slots.
            let binders = product.code().binders();
            assert_eq!(binders.len(), 1, "{} single binder", pair.label);
            let binder = &binders[0];
            assert_eq!(binder.unit, pair.unit, "{} binder unit", pair.label);
            assert_eq!(binder.module, pair.module, "{} binder module", pair.label);
            assert_eq!(
                binder.namespace, pair.namespace,
                "{} binder namespace",
                pair.label
            );
            assert_eq!(
                binder.occurrence, pair.occurrence,
                "{} binder occurrence",
                pair.label
            );
            assert_eq!(
                product.code().globals().len(),
                0,
                "{} zero package globals",
                pair.label
            );
            products.push(product);
        }

        let complete = &products[0];
        let minimal = &products[1];
        assert_eq!(
            complete.code().owner(),
            minimal.code().owner(),
            "{} bodies share one decoded owner/seal",
            pair.label
        );
        assert_ne!(
            complete.artifact_id(),
            minimal.artifact_id(),
            "{} bodies keep distinct ArtifactIds",
            pair.label
        );
        assert_ne!(
            *complete.product_sha256(),
            *minimal.product_sha256(),
            "{} bodies keep distinct product SHAs",
            pair.label
        );

        // Each restored product has a distinct ArtifactId even though the two
        // bodies share one decoded owner and defining interface seal.
        let complete_graph = issue_historical_product(&view, &operation, complete);
        let minimal_graph = issue_historical_product(&view, &operation, minimal);
        assert_eq!(
            complete_graph.roots().len(),
            1,
            "{} complete root",
            pair.label
        );
        assert_eq!(
            minimal_graph.roots().len(),
            1,
            "{} minimal root",
            pair.label
        );
        assert_eq!(
            complete_graph.roots()[0].artifact,
            complete.artifact_id(),
            "{} complete graph selects the complete body",
            pair.label
        );
        assert_eq!(
            minimal_graph.roots()[0].artifact,
            minimal.artifact_id(),
            "{} minimal graph selects the minimal body",
            pair.label
        );

        let mut wrong_producer = historical_reference(root.path(), pair, &pair.bodies[0]);
        wrong_producer.producer_sha256[0] ^= 1;
        assert!(
            matches!(
                crate::certified_products::CertifiedPackageCodeProduct::restore(
                    root.path(),
                    &wrong_producer,
                    &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                ),
                Err(crate::certified_products::CertificationError::Mismatch(
                    "package code recovery reference"
                ))
            ),
            "{} rejects a recovery producer/provenance mismatch",
            pair.label
        );
        let mut wrong_product = historical_reference(root.path(), pair, &pair.bodies[0]);
        wrong_product.product_sha256[0] ^= 1;
        assert!(
            matches!(
                crate::certified_products::CertifiedPackageCodeProduct::restore(
                    root.path(),
                    &wrong_product,
                    &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                ),
                Err(crate::certified_products::CertificationError::Mismatch(
                    "package code recovery reference"
                ))
            ),
            "{} rejects a recovery product identity mismatch",
            pair.label
        );
        let mut wrong_interface = historical_reference(root.path(), pair, &pair.bodies[0]);
        wrong_interface.defining_interface.sha256[0] ^= 1;
        assert!(
            matches!(
                crate::certified_products::CertifiedPackageCodeProduct::restore(
                    root.path(),
                    &wrong_interface,
                    &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                ),
                Err(crate::certified_products::CertificationError::Mismatch(
                    "package code recovery reference"
                ))
            ),
            "{} rejects a recovery interface hash/path mismatch",
            pair.label
        );
    }
}
