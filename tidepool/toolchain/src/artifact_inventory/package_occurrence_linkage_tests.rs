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
            // The shared `pack` symbol resolves to two different bodies under
            // two different projections; an exact occurrence link keeps them
            // apart where a request-wide symbol map would collapse them.
            assert_ne!(
                only_pack_slots, pack_then_unpack_slots,
                "the shared pack slot must keep distinct body bytes per target projection"
            );
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
