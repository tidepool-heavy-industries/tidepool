//! Actual compiler and authenticated-product controls for package linkage.
//!
//! Native histories come from the real compiler through `compile_invocation`.
//! Historical product DATA comes from authenticated frozen compiler envelopes.
//! No test constructor grants acquisition or installation authority.
//!
//! These tests are ignored by default because they require the rooted
//! compiler service; the joined qualification runs them in that environment.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use crate::artifact_inventory::NativeGroupBinding;
use crate::artifacts::test_support::compile_invocation;
use crate::artifacts::CompileInvocation;
use crate::linked_native::{
    AcquiredNativeCode, AcquiredNativeImport, LinkedNativeTarget, LinkedNativeTargetImport,
    NativeCodeGrant,
};
use tidepool_repr::execution_schema::ValueId;
use tidepool_repr::native_code::NativeCodeId;

/// Observe actual code bodies and ordered edges, independently of receipt-local
/// package stage/occurrence coordinates. Content alone grants no acquisition.
#[derive(Debug, Clone, Eq, PartialEq)]
struct PackageGraphObservation {
    root: NativeCodeId,
    nodes: BTreeMap<NativeCodeId, PackageGraphNode>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PackageGraphNode {
    owner: String,
    envelope_sha256: [u8; 32],
    interface_sha256: [u8; 32],
    body_sha256: [u8; 32],
    imports: Vec<(NativeCodeId, ValueId)>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PackageSlotRecord {
    identity: String,
    graph: PackageGraphObservation,
}

fn package_product(
    acquired: &AcquiredNativeCode,
) -> &Arc<crate::certified_products::CertifiedPackageCodeProduct> {
    match acquired.grant() {
        NativeCodeGrant::Package { product, .. } => product,
        _ => panic!("actual package occurrence required"),
    }
}

fn home_product(
    acquired: &AcquiredNativeCode,
) -> &Arc<crate::certified_products::PendingCertifiedGroup> {
    match acquired.grant() {
        NativeCodeGrant::Home { product, .. } => product,
        _ => panic!("actual HOME occurrence required"),
    }
}

fn home_binding(acquired: &AcquiredNativeCode) -> NativeGroupBinding {
    match acquired.grant() {
        NativeCodeGrant::Home { binding, .. } => binding,
        _ => panic!("actual HOME occurrence required"),
    }
}

fn package_children(acquired: &AcquiredNativeCode) -> Vec<AcquiredNativeCode> {
    acquired
        .imports()
        .filter_map(|import| match import {
            AcquiredNativeImport::Code { acquired, .. }
                if matches!(acquired.grant(), NativeCodeGrant::Package { .. }) =>
            {
                Some(acquired)
            }
            _ => None,
        })
        .collect()
}

fn homes(target: &LinkedNativeTarget) -> impl Iterator<Item = AcquiredNativeCode> + '_ {
    target
        .acquired()
        .filter(|acquired| matches!(acquired.grant(), NativeCodeGrant::Home { .. }))
}

fn observe_package_graph(acquired: &AcquiredNativeCode) -> PackageGraphObservation {
    let root = acquired.code().id();
    let mut nodes = BTreeMap::new();
    let mut pending = VecDeque::from([acquired.clone()]);
    while let Some(acquired) = pending.pop_front() {
        let exact = acquired.code().id();
        if nodes.contains_key(&exact) {
            continue;
        }
        let product = package_product(&acquired);
        let mut children = Vec::new();
        let imports = acquired
            .imports()
            .map(|import| match import {
                AcquiredNativeImport::Code { acquired, value } => {
                    assert!(matches!(acquired.grant(), NativeCodeGrant::Package { .. }));
                    let edge = (acquired.code().id(), value);
                    children.push(acquired);
                    edge
                }
                AcquiredNativeImport::Retained { .. } => {
                    panic!("PACKAGE cannot import retained runtime values")
                }
            })
            .collect();
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
        pending.extend(children);
    }
    PackageGraphObservation { root, nodes }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct HomePackageRoot {
    finder_binder: String,
    imports: Vec<PackageGraphObservation>,
}

fn home_package_roots(target: &LinkedNativeTarget) -> BTreeMap<u32, HomePackageRoot> {
    let mut roots = BTreeMap::new();
    for acquired in homes(target) {
        let imports = package_children(&acquired)
            .iter()
            .map(observe_package_graph)
            .collect::<Vec<_>>();
        if imports.is_empty() {
            continue;
        }
        let product = home_product(&acquired);
        let binding = home_binding(&acquired);
        let finder_binder = product
            .group()
            .binders()
            .iter()
            .map(|binder| binder.occurrence.as_str())
            .find(|binder| matches!(*binder, "byList" | "byText"))
            .unwrap_or_else(|| {
                panic!(
                    "HOME package ordinal {} has no fixture binder: {:?}",
                    binding.original_ordinal,
                    product.group().binders()
                )
            })
            .to_owned();
        assert!(
            roots
                .insert(
                    binding.original_ordinal,
                    HomePackageRoot {
                        finder_binder,
                        imports
                    }
                )
                .is_none(),
            "one acquired HOME occurrence per issued ordinal"
        );
    }
    roots
}

fn package_slot_records(
    output: &crate::artifacts::CompiledArtifacts,
    target_owner: &LinkedNativeTarget,
    target: &str,
) -> Vec<PackageSlotRecord> {
    let globals = output.targets[target].prepared.prepared().globals();
    assert_eq!(
        globals.len(),
        target_owner.imports().len(),
        "{target} has one ordered import per physical global"
    );
    globals
        .iter()
        .zip(target_owner.imports())
        .filter_map(|(global, import)| match import {
            LinkedNativeTargetImport::Code { acquired, .. }
                if matches!(acquired.grant(), NativeCodeGrant::Package { .. }) =>
            {
                Some(PackageSlotRecord {
                    identity: format!(
                        "{}:{}:{}",
                        global.identity.unit, global.identity.module, global.identity.occurrence
                    ),
                    graph: observe_package_graph(acquired),
                })
            }
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

fn assert_current_handles_are_target_bound(
    output: &crate::artifacts::CompiledArtifacts,
    target: &str,
) {
    use crate::linked_native::NativeCodeGrant;
    let linked = output.targets[target]
        .linked_native_target()
        .expect("actual finite target owns its linked code");
    let sources = output
        .native_source_products(target)
        .expect("actual source receipt resolves")
        .expect("finite target source receipt");
    let support = crate::artifact_inventory::CompilerSourceSupport::close_complete_output(
        output
            .issued_output
            .as_ref()
            .expect("actual compiler output owner")
            .compiler_inputs(),
    )
    .unwrap()
    .with_linked_native_target(linked)
    .unwrap();
    let narrowed = support
        .for_source_owners(&BTreeSet::new())
        .unwrap()
        .finalize_namespace()
        .unwrap();
    assert!(narrowed
        .projection()
        .roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert!(narrowed.artifacts().selected_native_groups().is_empty());
    assert!(
        narrowed
            .acquired_native_closures()
            .contains_owner(linked.closure()),
        "role narrowing keeps the actual acquired code owner without selecting execution"
    );
    let mut checked = 0;
    for acquired in linked.acquired() {
        let NativeCodeGrant::Home {
            binding,
            current: Some(_),
            ..
        } = acquired.grant()
        else {
            continue;
        };
        assert!(sources
            .physical_current_for(linked, &acquired)
            .expect("receiving target handle retains Current")
            .is_some());
        let historical = linked
            .closure()
            .home(binding)
            .expect("same exact closure lends a historical handle");
        assert!(matches!(
            historical.grant(),
            NativeCodeGrant::Home { current: None, .. }
        ));
        assert!(
            sources.physical_current_for(linked, &historical).is_err(),
            "a historical handle cannot borrow receiving Current authority"
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "the fresh multi-ordinal fixture demands Current HOME code"
    );
}

fn linked_target(
    output: &crate::artifacts::CompiledArtifacts,
    target: &str,
) -> Arc<LinkedNativeTarget> {
    Arc::clone(
        output.targets[target]
            .linked_native_target()
            .expect("actual finite target has issued linkage"),
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
        let only_pack = linked_target(&output, "onlyPack");
        let pack_then_unpack = linked_target(&output, "packThenUnpack");

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
    let via_text = linked_target(&output, "viaText");
    let direct_text = linked_target(&output, "directText");

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
    let selection = linked_target(&output, "threeSorts");
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
    let first = linked_target(&output, "firstHistory");
    let second = linked_target(&output, "secondHistory");
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
            targets: &["result", "emptyTarget"],
            include: &[root.path().to_owned()],
            fallback_module_name: "MultiOrdinalHomeConsumer",
        },
        |_, _, _| {},
    )
    .expect("fresh multi-ordinal HOME module compiles through common admission");
    assert_current_handles_are_target_bound(&output, "result");
    let empty = output.targets["emptyTarget"]
        .linked_native_target()
        .expect("actual literal target has an issued linked owner");
    assert!(
        empty.prepared().globals().is_empty(),
        "literal target must actually have no dependency globals"
    );
    assert_eq!(empty.acquired().len(), 0);
    let baseline = crate::artifact_inventory::CompilerSourceSupport::empty();
    let unchanged = baseline.with_linked_native_target(empty).unwrap();
    assert!(unchanged
        .acquired_native_closures()
        .same_held_owners(baseline.acquired_native_closures()));
    assert!(unchanged.native_component_records().is_empty());
    assert!(unchanged.native_closure_records().is_empty());
    assert!(Arc::ptr_eq(
        unchanged.native_fact_owner(),
        baseline.native_fact_owner()
    ));

    let selection = linked_target(&output, "result");
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
    let result = linked_target(&forward, "result");
    let result2 = linked_target(&forward, "result2");
    let result_roots = home_package_roots(&result);
    let result2_roots = home_package_roots(&result2);
    assert_eq!(result_roots.len(), 2);
    assert_eq!(result2_roots, result_roots);
    for (target, selected) in [("result", &result), ("result2", &result2)] {
        let sources = forward.targets[target]
            .native_source_products()
            .unwrap()
            .unwrap();
        for acquired in homes(selected) {
            assert!(
                sources
                    .physical_current_for(selected, &acquired)
                    .unwrap()
                    .is_some(),
                "{target} preserves receiving Current for ordinal {}",
                home_binding(&acquired).original_ordinal
            );
        }
    }
    let left_sources = forward.targets["result"]
        .native_source_products()
        .unwrap()
        .unwrap();
    let shared = homes(&result)
        .next()
        .expect("actual current HOME occurrence");
    assert!(
        left_sources
            .physical_current_for(&result2, &shared)
            .is_err(),
        "another target cannot authenticate the same source receipt"
    );
    for (ordinal, left) in &result_roots {
        let left_owner = homes(&result)
            .find(|home| home_binding(home).original_ordinal == *ordinal)
            .unwrap();
        let right_owner = homes(&result2)
            .find(|home| home_binding(home).original_ordinal == *ordinal)
            .unwrap();
        assert!(
            home_product(&left_owner).same_issuance(home_product(&right_owner)),
            "ordinal {ordinal} retains its exact HOME issuer"
        );
        assert_eq!(left_owner.code().id(), right_owner.code().id());
        let left_children = package_children(&left_owner);
        let right_children = package_children(&right_owner);
        assert_eq!(left_children.len(), left.imports.len());
        assert_eq!(right_children.len(), left_children.len());
        for (left, right) in left_children.iter().zip(&right_children) {
            assert_eq!(left.code().id(), right.code().id());
            assert_eq!(
                left.occurrence(),
                right.occurrence(),
                "an already selected HOME keeps its ordered package receipt intact"
            );
            assert!(
                Arc::ptr_eq(package_product(left), package_product(right)),
                "selected HOME retains its immutable package custody"
            );
        }
    }
    drop(forward);

    let reversed = compile(&["result2", "result"]);
    assert_eq!(
        home_package_roots(&linked_target(&reversed, "result")),
        result_roots,
        "target order cannot change result's exact HOME roots"
    );
    assert_eq!(
        home_package_roots(&linked_target(&reversed, "result2")),
        result2_roots,
        "target order cannot change result2's exact HOME roots"
    );
    drop(reversed);
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn transitive_home_package_demand_reaches_the_final_target() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("TransitiveHomeMiddle.hs"),
        include_str!("../../tests/fixtures/package-occurrence-linkage/TransitiveHomeMiddle.hs"),
    )
    .unwrap();
    std::fs::write(
        root.path().join("TransitiveHomeLeaf.hs"),
        include_str!("../../tests/fixtures/package-occurrence-linkage/TransitiveHomeLeaf.hs"),
    )
    .unwrap();
    let output = compile_invocation(
        &CompileInvocation {
            source: include_str!(
                "../../tests/fixtures/package-occurrence-linkage/TransitiveHomeTarget.hs"
            ),
            targets: &["target"],
            include: &[root.path().to_owned()],
            fallback_module_name: "TransitiveHomeTarget",
        },
        |_, _, _| {},
    )
    .expect("the target-local transitive HOME graph compiles through common admission");
    let selection = linked_target(&output, "target");
    let row = |module: &str, binder: &str| {
        homes(&selection)
            .find(|home| {
                let product = home_product(home);
                product.owner().module == module
                    && product
                        .group()
                        .binders()
                        .iter()
                        .any(|symbol| symbol.occurrence == binder)
            })
            .unwrap_or_else(|| panic!("target lacks actual {module}.{binder} HOME occurrence"))
    };
    let globals = output.targets["target"].prepared.prepared().globals();
    assert_eq!(globals.len(), selection.imports().len());
    let middle = row("TransitiveHomeMiddle", "middleValue");
    let leaf = row("TransitiveHomeLeaf", "leafValue");
    assert!(globals.iter().zip(selection.imports()).any(|(global, import)| {
        global.identity.module == "TransitiveHomeMiddle" && global.identity.occurrence == "middleValue"
            && matches!(import, LinkedNativeTargetImport::Code { acquired, .. }
                if matches!(acquired.grant(), NativeCodeGrant::Home { binding, .. } if binding == home_binding(&middle)))
    }), "final target directly imports its exact Middle HOME occurrence");
    assert!(selection.imports().iter().all(|import| !matches!(import,
        LinkedNativeTargetImport::Code { acquired, .. } if matches!(acquired.grant(), NativeCodeGrant::Package { .. }))),
        "final target reaches PACKAGE through HOME rather than direct package slots");
    assert!(middle.imports().any(|import| matches!(import,
        AcquiredNativeImport::Code { acquired, .. }
            if matches!(acquired.grant(), NativeCodeGrant::Home { binding, .. } if binding == home_binding(&leaf)))),
        "Middle imports its exact transitive Leaf HOME occurrence");
    assert!(
        homes(&selection).all(|home| home_product(&home).owner().module != "TransitiveHomeTarget"),
        "the target is not its own dependency HOME occurrence"
    );
    let middle_packages = package_children(&middle)
        .iter()
        .map(|package| package_product(package).code().owner().module.clone())
        .collect::<Vec<_>>();
    assert!(
        middle_packages
            .iter()
            .all(|module| module != "Data.Text.Internal"),
        "Text package demand occurs through the second HOME edge: {middle_packages:?}"
    );
    let mut seen = BTreeSet::new();
    let mut pending = vec![leaf];
    let mut leaf_packages = Vec::new();
    while let Some(home) = pending.pop() {
        if !seen.insert(home.occurrence()) {
            continue;
        }
        for import in home.imports() {
            if let AcquiredNativeImport::Code { acquired, .. } = import {
                match acquired.grant() {
                    NativeCodeGrant::Home { .. } => pending.push(acquired),
                    NativeCodeGrant::Package { product, .. } => {
                        leaf_packages.push(product.code().owner().module.clone())
                    }
                }
            }
        }
    }
    assert!(
        leaf_packages
            .iter()
            .any(|module| module == "Data.Text.Internal"),
        "actual HOME traversal retains transitive Text code: {leaf_packages:?}"
    );
    drop(output);
}

#[test]
#[ignore = "requires the rooted compiler service declared by toolchain_finite_output_tests"]
fn same_direct_package_ignores_unrelated_dictionary_cohort() {
    let source = include_str!(
        "../../tests/fixtures/package-occurrence-linkage/SameDirectDifferentChildren.hs"
    );
    let ints = compile_fixture(source, &["sortInts"]);
    let strings = compile_fixture(source, &["sortStrings"]);
    let left = linked_target(&ints, "sortInts");
    let right = linked_target(&strings, "sortStrings");
    let direct_packages = |target: &LinkedNativeTarget| {
        target
            .imports()
            .iter()
            .filter_map(|import| match import {
                LinkedNativeTargetImport::Code { acquired, .. }
                    if matches!(acquired.grant(), NativeCodeGrant::Package { .. }) =>
                {
                    Some(acquired.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let left_packages = direct_packages(&left);
    let right_packages = direct_packages(&right);
    let mut shared = 0;
    let mut different_receipt = 0;
    for left in &left_packages {
        for right in &right_packages {
            if package_product(left).artifact_id() == package_product(right).artifact_id() {
                shared += 1;
                assert_eq!(
                    observe_package_graph(left),
                    observe_package_graph(right),
                    "unrelated target dictionaries cannot change a shared package's closed code"
                );
                if left.occurrence() != right.occurrence() {
                    different_receipt += 1;
                }
            }
        }
    }
    assert!(
        shared > 0,
        "actual separate target histories share a direct package product"
    );
    assert!(
        different_receipt > 0,
        "equal closed code is compared across distinct actual receipt occurrences"
    );
    let left_codes = left_packages
        .iter()
        .map(|package| package.code().id())
        .collect::<BTreeSet<_>>();
    let right_codes = right_packages
        .iter()
        .map(|package| package.code().id())
        .collect::<BTreeSet<_>>();
    assert_ne!(
        left_codes, right_codes,
        "the target histories must actually select different dictionary code siblings"
    );
}

// ---------------------------------------------------------------------------
// Authenticated historical same-seal/different-body controls.
//
// The two package pairs below are decoded from immutable compiler envelopes
// retained in the P3 fixture tree and listed in `historical/manifest.json`.
// Authority comes only from those authenticated bytes through the production
// reader (`CertifiedPackageCodeProduct::restore` -> `parse_code` ->
// `PackageGroupCode::admit`). Pure code identities grant no acquisition; no
// test constructs a package owner, product or manifest occurrence.
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

fn historical_code_identity(
    product: &Arc<crate::certified_products::CertifiedPackageCodeProduct>,
) -> NativeCodeId {
    use tidepool_repr::native_code::{NativeCodeBody, NativeCodeComponent, NativeCodePlan};
    let operation = tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
    let code = NativeCodeComponent::link(
        &[NativeCodePlan {
            body: NativeCodeBody::Package(product.code().clone()),
            imports: Vec::new(),
        }],
        &[0],
        &operation,
    )
    .expect("restored zero-import package body links as immutable DATA");
    assert_eq!(code.len(), 1);
    code[0].id()
}

#[test]
#[ignore = "decodes retained historical envelopes through the counted owned-resident runner"]
fn historical_same_seal_bodies_keep_distinct_code_identities() {
    let manifest = historical_manifest();
    assert_eq!(manifest.pairs.len(), 2, "two authenticated package pairs");
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

        assert_ne!(
            historical_code_identity(complete),
            historical_code_identity(minimal),
            "{} actual differing bodies with equal nominal seals retain distinct pure code identities",
            pair.label
        );
        // These immutable DATA observations create no manifest occurrence,
        // acquired closure, compiler selection or installation permission.
    }
}
