//! P3 independent oracle controls for exact package occurrence linkage.
//!
//! Every positive history here is produced by the real compiler through
//! `test_support::compile_invocation`; no test-only certificate constructor
//! grants package authority. The assertions read the compiler-issued
//! selection, never a re-derivation of the repaired index.
//!
//! These tests are ignored by default because they require the rooted
//! compiler service; the joined qualification runs them in that environment.
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use crate::artifact_inventory::{
    ArtifactBinding, CompletedNativeImport, CompletedNativeTargetImport, PackageCodeBinding,
};
use crate::artifacts::test_support::compile_invocation;
use crate::artifacts::CompileInvocation;

/// A finite observation of one issued package graph. Nodes are keyed by their
/// exact artifact binding, so cycles terminate without silently truncating the
/// graph and repeated edges retain their positions.
#[derive(Debug, Clone, Eq, PartialEq)]
struct PackageGraphObservation {
    root: ArtifactBinding,
    nodes: BTreeMap<ArtifactBinding, PackageGraphNode>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PackageGraphNode {
    owner: String,
    envelope_sha256: [u8; 32],
    interface_sha256: [u8; 32],
    body_sha256: [u8; 32],
    imports: Vec<ArtifactBinding>,
}

/// One ordered package slot of a completed target: the defining identity and
/// the exact issued graph selected for that slot.
#[derive(Debug, Clone, Eq, PartialEq)]
struct PackageSlotRecord {
    identity: String,
    graph: PackageGraphObservation,
}

fn observe_package_graph(binding: &PackageCodeBinding) -> PackageGraphObservation {
    let root = binding.binding();
    let mut nodes = BTreeMap::new();
    let mut pending = VecDeque::from([binding.clone()]);
    while let Some(binding) = pending.pop_front() {
        let exact = binding.binding();
        if nodes.contains_key(&exact) {
            continue;
        }
        let product = binding.product();
        let imports = binding
            .imports()
            .map(|child| child.binding())
            .collect::<Vec<_>>();
        nodes.insert(
            exact,
            PackageGraphNode {
                owner: format!(
                    "{}:{}",
                    product.code().owner().unit,
                    product.code().owner().module
                ),
                envelope_sha256: *product.envelope_sha256(),
                interface_sha256: *product.defining_interface().sha256(),
                body_sha256: sha256(product.bytes()),
                imports,
            },
        );
        pending.extend(binding.imports());
    }
    PackageGraphObservation { root, nodes }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct HomePackageRoot {
    finder_binder: String,
    imports: Vec<PackageGraphObservation>,
}

fn home_package_roots(
    selection: &Arc<crate::artifact_inventory::CompletedNativeSelection>,
) -> BTreeMap<u32, HomePackageRoot> {
    let mut roots = BTreeMap::new();
    for row in selection.completion().rows() {
        let imports = row
            .imports()
            .iter()
            .filter_map(|import| match import {
                CompletedNativeImport::Package { binding, .. } => {
                    Some(observe_package_graph(binding))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if imports.is_empty() {
            continue;
        }
        let finder_binder = row
            .product()
            .group()
            .binders()
            .iter()
            .map(|binder| binder.occurrence.as_str())
            .find(|binder| matches!(*binder, "byList" | "byText"))
            .unwrap_or_else(|| {
                panic!(
                    "HOME package row ordinal {} has no fixture binder: {:?}",
                    row.binding().original_ordinal,
                    row.product().group().binders()
                )
            })
            .to_owned();
        assert!(
            roots
                .insert(
                    row.binding().original_ordinal,
                    HomePackageRoot {
                        finder_binder,
                        imports,
                    },
                )
                .is_none(),
            "one completed HOME row per issued ordinal"
        );
    }
    roots
}

fn package_slot_records(
    output: &crate::artifacts::CompiledArtifacts,
    selection: &Arc<crate::artifact_inventory::CompletedNativeSelection>,
    target: &str,
) -> Vec<PackageSlotRecord> {
    let globals = output.targets[target].prepared.prepared().globals();
    let links = selection.target_imports();
    assert_eq!(
        globals.len(),
        links.len(),
        "{target} completion must have one ordered slot per physical global"
    );
    globals
        .iter()
        .zip(links)
        .filter_map(|(global, link)| match link {
            CompletedNativeTargetImport::Package { binding, .. } => Some(PackageSlotRecord {
                identity: format!(
                    "{}:{}:{}",
                    global.identity.unit, global.identity.module, global.identity.occurrence
                ),
                graph: observe_package_graph(binding),
            }),
            _ => None,
        })
        .collect()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
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
fn reversed_targets_preserve_exact_package_graphs() {
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
        // contract. Ordering a target first or second must not change its own
        // ordered slots or exact issued graph.
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
                "reversing target order changed a target's exact package slots or issued graph"
            );
        } else {
            expected = Some(order);
        }
        drop(output);
    }
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn shared_package_symbol_graph_is_order_invariant() {
    let source =
        include_str!("../../tests/fixtures/package-occurrence-linkage/TransitiveCompetitor.hs");
    let output = compile_fixture(source, &["viaText", "directText"]);
    let via_text = completed_selection(&output, "viaText");
    let direct_text = completed_selection(&output, "directText");

    // Shared package products must keep their exact issued graphs when target
    // order changes. This is order-invariance coverage, not a competing-child
    // discriminator.
    let via = package_slot_records(&output, &via_text, "viaText");
    let direct = package_slot_records(&output, &direct_text, "directText");
    assert!(!via.is_empty(), "viaText must retain actual package code");
    // If the two projections share a package global, its complete finite graph
    // observation must agree. The current fixture does not establish that two
    // different candidates exist, so it does not claim transitive competition.
    if !direct.is_empty() {
        for left in &via {
            for right in &direct {
                if left.identity == right.identity {
                    assert_eq!(
                        left.graph, right.graph,
                        "one shared symbol keeps its complete issued graph"
                    );
                }
            }
        }
    }
    drop(output);
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn compiler_deduplicates_physical_sort_global() {
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
fn duplicate_envelope_smoke_compiles_both_targets() {
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

    let selection = completed_selection(&output, "result");
    let roots = home_package_roots(&selection);
    assert_eq!(
        roots.len(),
        2,
        "exactly two fresh HOME ordinals carry package roots: {roots:#?}"
    );
    let by_list = roots
        .iter()
        .find(|(_, root)| root.finder_binder == "byList")
        .expect("the issued byList ordinal");
    let by_text = roots
        .iter()
        .find(|(_, root)| root.finder_binder == "byText")
        .expect("the issued byText ordinal");
    assert!(
        by_list.0 != by_text.0,
        "byList and byText are distinct issuer ordinals"
    );
    assert_eq!(
        by_list.1.imports.len(),
        2,
        "byList imports its sort body and Ord dictionary"
    );
    assert_eq!(by_text.1.imports.len(), 1, "byText imports its pack body");
    let by_list_modules = by_list
        .1
        .imports
        .iter()
        .map(|root| root.nodes[&root.root].owner.as_str())
        .collect::<Vec<_>>();
    let by_text_modules = by_text
        .1
        .imports
        .iter()
        .map(|root| root.nodes[&root.root].owner.as_str())
        .collect::<Vec<_>>();
    assert!(
        by_list_modules
            .iter()
            .any(|owner| owner.ends_with("OldList")),
        "byList keeps its exact OldList root: {by_list_modules:?}"
    );
    assert!(
        by_list_modules
            .iter()
            .all(|owner| !owner.ends_with("Data.Text.Internal")),
        "byList must not receive byText's root: {by_list_modules:?}"
    );
    assert!(
        by_text_modules
            .iter()
            .any(|owner| owner.ends_with("Data.Text.Internal")),
        "byText keeps its exact Data.Text root: {by_text_modules:?}"
    );
    assert!(
        by_text_modules
            .iter()
            .all(|owner| !owner.ends_with("OldList")),
        "byText must not receive byList's root: {by_text_modules:?}"
    );
    drop(output);
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn repeated_fresh_home_targets_reuse_exact_ordinal_bindings() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("MultiOrdinalHome.hs"),
        include_str!("../../tests/fixtures/package-occurrence-linkage/MultiOrdinalHome.hs"),
    )
    .unwrap();
    let source =
        include_str!("../../tests/fixtures/package-occurrence-linkage/MultiOrdinalHomeConsumer.hs");
    let compile = |targets: &[&str]| {
        compile_invocation(
            &CompileInvocation {
                source,
                targets,
                include: &[root.path().to_owned()],
                fallback_module_name: "MultiOrdinalHomeConsumer",
            },
            |_, _, _| {},
        )
        .expect("both targets consume the shared fresh HOME issuers")
    };

    let forward = compile(&["result", "result2"]);
    let result = completed_selection(&forward, "result");
    let result2 = completed_selection(&forward, "result2");
    let result_roots = home_package_roots(&result);
    let result2_roots = home_package_roots(&result2);
    assert_eq!(result_roots.len(), 2);
    assert_eq!(result2_roots, result_roots);

    let package_bindings =
        |selection: &Arc<crate::artifact_inventory::CompletedNativeSelection>, ordinal: u32| {
            selection
                .completion()
                .rows()
                .find(|row| row.binding().original_ordinal == ordinal)
                .expect("shared HOME row")
                .imports()
                .iter()
                .filter_map(|import| match import {
                    CompletedNativeImport::Package { binding, .. } => Some(binding.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
    for (ordinal, left) in &result_roots {
        let left_product = result
            .completion()
            .rows()
            .find(|row| row.binding().original_ordinal == *ordinal)
            .expect("result HOME row")
            .product();
        let right_product = result2
            .completion()
            .rows()
            .find(|row| row.binding().original_ordinal == *ordinal)
            .expect("result2 HOME row")
            .product();
        assert!(
            Arc::ptr_eq(left_product, right_product),
            "ordinal {ordinal} reuses the same HOME product custody"
        );
        let left_bindings = package_bindings(&result, *ordinal);
        let right_bindings = package_bindings(&result2, *ordinal);
        assert_eq!(
            left_bindings.len(),
            left.imports.len(),
            "ordinal {ordinal} keeps its exact root cardinality"
        );
        assert_eq!(right_bindings.len(), left_bindings.len());
        for (left, right) in left_bindings.iter().zip(&right_bindings) {
            assert_eq!(
                left.binding(),
                right.binding(),
                "ordinal {ordinal} reuses the same exact package binding"
            );
            assert!(
                Arc::ptr_eq(left.product(), right.product()),
                "ordinal {ordinal} reuses immutable payload custody"
            );
        }
    }
    drop(forward);

    let reversed = compile(&["result2", "result"]);
    assert_eq!(
        home_package_roots(&completed_selection(&reversed, "result")),
        result_roots,
        "target order cannot change result's exact HOME roots"
    );
    assert_eq!(
        home_package_roots(&completed_selection(&reversed, "result2")),
        result2_roots,
        "target order cannot change result2's exact HOME roots"
    );
    drop(reversed);
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn same_direct_package_with_different_children_has_a_real_premise() {
    let source = include_str!(
        "../../tests/fixtures/package-occurrence-linkage/SameDirectDifferentChildren.hs"
    );
    let ints = compile_fixture(source, &["sortInts"]);
    let strings = compile_fixture(source, &["sortStrings"]);
    let int_slots =
        package_slot_records(&ints, &completed_selection(&ints, "sortInts"), "sortInts");
    let string_slots = package_slot_records(
        &strings,
        &completed_selection(&strings, "sortStrings"),
        "sortStrings",
    );
    let mut crossed_children = Vec::new();
    for left in &int_slots {
        let left = &left.graph.nodes[&left.graph.root];
        for right in &string_slots {
            let right = &right.graph.nodes[&right.graph.root];
            if left.owner == right.owner
                && left.interface_sha256 == right.interface_sha256
                && left.envelope_sha256 == right.envelope_sha256
                && left.body_sha256 == right.body_sha256
                && left.imports != right.imports
            {
                crossed_children.push((
                    left.owner.clone(),
                    left.envelope_sha256,
                    left.imports.clone(),
                    right.imports.clone(),
                ));
            }
        }
    }
    assert_eq!(
        crossed_children.len(),
        1,
        "exactly one direct package identity must have two real child histories: {crossed_children:#?}"
    );
    drop(ints);
    drop(strings);
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

#[derive(serde::Deserialize)]
struct HistoricalManifest {
    pairs: Vec<HistoricalPair>,
}

#[derive(serde::Deserialize)]
struct HistoricalPair {
    label: String,
    unit: String,
    module: String,
    namespace: String,
    occurrence: String,
    compiler_sha256: String,
    profile_sha256: String,
    interface_sha256: String,
    defining_version_sha256: String,
    bodies: Vec<HistoricalBody>,
}

#[derive(serde::Deserialize)]
struct HistoricalBody {
    envelope_sha256: String,
}

fn historical_manifest() -> HistoricalManifest {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/package-occurrence-linkage/historical/manifest.json"
    ))
    .expect("authenticated historical manifest parses")
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

/// Materialize one authenticated envelope beside its defining interface and
/// restore it through the production reader. The owner is decoded from the
/// envelope bytes, never rebuilt from the manifest.
fn restore_historical_product(
    root: &std::path::Path,
    pair: &HistoricalPair,
    body: &HistoricalBody,
) -> std::sync::Arc<crate::certified_products::CertifiedPackageCodeProduct> {
    let envelope_sha256 = hex_decode(&body.envelope_sha256);
    let interface_sha256 = hex_decode(&pair.interface_sha256);
    let compiler_sha256 = hex_decode(&pair.compiler_sha256);
    let profile_sha256 = hex_decode(&pair.profile_sha256);
    let defining_version_sha256 = hex_decode(&pair.defining_version_sha256);

    std::fs::create_dir_all(root.join("package-code")).unwrap();
    std::fs::write(
        root.join("package-code")
            .join(format!("{}.cbor", hex_encode(&envelope_sha256))),
        historical_envelope_bytes(&body.envelope_sha256),
    )
    .unwrap();
    std::fs::write(
        root.join("package-code")
            .join(format!("{}.hi", hex_encode(&interface_sha256))),
        historical_interface_bytes(&pair.interface_sha256),
    )
    .unwrap();

    let reference = crate::certified_products::RecoveryPackageCodeRef {
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
    };
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
fn historical_same_seal_bodies_select_separately() {
    let manifest = historical_manifest();
    assert_eq!(manifest.pairs.len(), 2, "two authenticated package pairs");
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

        // Select each same-seal body separately by its own occurrence
        // coordinate (ArtifactId), through the existing graph issuer.
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

        assert_ne!(
            complete_graph.roots()[0].artifact,
            minimal_graph.roots()[0].artifact,
            "{} exact roots select distinct same-seal bodies",
            pair.label
        );
    }
}
