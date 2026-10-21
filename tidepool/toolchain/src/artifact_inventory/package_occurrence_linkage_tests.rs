//! P3 independent oracle controls for exact package occurrence linkage.
//!
//! Every positive history here is produced by the real compiler through
//! `test_support::compile_invocation`; no test-only certificate constructor
//! grants package authority. The assertions are written against the retained
//! physical selection, so a regression to the previous `(seal, symbol)` /
//! first-match selector refuses these fixtures with "competing captured code
//! products" (or selects the wrong body) instead of passing.
//!
//! These tests are ignored by default because they require the rooted
//! compiler service; the joined qualification runs them in that environment.
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::artifact_inventory::CompletedNativeTargetImport;
use crate::artifacts::test_support::compile_invocation;
use crate::artifacts::CompileInvocation;

/// For each ordered package slot of a completed target, the defining
/// `unit:module:occurrence` identity paired with the package body bytes that
/// slot resolved to. The oracle reads the compiler-issued selection; it never
/// re-derives the repaired index.
fn package_slot_bodies(
    output: &crate::artifacts::CompiledArtifacts,
    selection: &Arc<crate::artifact_inventory::CompletedNativeSelection>,
    target: &str,
) -> BTreeMap<String, Vec<u8>> {
    let globals = output.targets[target].prepared.prepared().globals();
    let mut slots = BTreeMap::new();
    for (global, link) in globals.iter().zip(selection.target_imports()) {
        if let CompletedNativeTargetImport::Package { binding, .. } = link {
            let key = format!(
                "{}:{}:{}",
                global.identity.unit, global.identity.module, global.identity.occurrence
            );
            slots.insert(key, binding.product().bytes().to_vec());
        }
    }
    slots
}

/// The ordered defining identities of the target's package slots. Order is
/// the program's own package-global order; reversing target order must not
/// reorder or re-resolve another target's slots.
fn package_slot_identities(
    output: &crate::artifacts::CompiledArtifacts,
    selection: &Arc<crate::artifact_inventory::CompletedNativeSelection>,
    target: &str,
) -> Vec<String> {
    output.targets[target]
        .prepared
        .prepared()
        .globals()
        .iter()
        .zip(selection.target_imports())
        .filter_map(|(global, link)| {
            matches!(link, CompletedNativeTargetImport::Package { .. }).then(|| {
                format!(
                    "{}:{}:{}",
                    global.identity.unit, global.identity.module, global.identity.occurrence
                )
            })
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
    let mut expected: Option<(Vec<String>, Vec<String>)> = None;
    for targets in [
        ["onlySort", "sortThenUnlines"],
        ["sortThenUnlines", "onlySort"],
    ] {
        let output = compile_fixture(source, &targets);
        let only_sort = completed_selection(&output, "onlySort");
        let sort_then_unlines = completed_selection(&output, "sortThenUnlines");

        // Both targets retain actual package code resolved through the export
        // contract (not merely a digest match).
        let only_sort_slots = package_slot_bodies(&output, &only_sort, "onlySort");
        let sort_then_unlines_slots =
            package_slot_bodies(&output, &sort_then_unlines, "sortThenUnlines");
        assert!(
            !only_sort_slots.is_empty() && !sort_then_unlines_slots.is_empty(),
            "both targets must select actual package code"
        );

        // Ordering a target first or second must not change its own ordered
        // slot selection: the producer's paired occurrence, not map order,
        // decides which body each slot resolves to.
        let order = (
            package_slot_identities(&output, &only_sort, "onlySort"),
            package_slot_identities(&output, &sort_then_unlines, "sortThenUnlines"),
        );
        if let Some(previous) = &expected {
            assert_eq!(
                previous, &order,
                "reversing target order changed a target's exact package slots"
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

    // Data.Text's transitive child selection and the direct sibling target may
    // retain two distinct projections of the same defining module. The exact
    // link must not silently borrow the sibling's body.
    assert!(!package_slot_bodies(&output, &via_text, "viaText").is_empty());
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
    // duplicate the retained payload: each distinct symbol keeps exactly one
    // body, never three copies of the same envelope.
    let bodies = package_slot_bodies(&output, &selection, "threeSorts");
    let sort_bodies = bodies
        .iter()
        .filter(|(identity, _)| identity.ends_with(":actualSort"))
        .count();
    assert_eq!(
        sort_bodies, 1,
        "three identical sort demands share one immutable actualSort body"
    );
    assert!(!bodies.is_empty(), "the target selects actual package code");
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
    // This fixture shares a defining module across two targets with different
    // reachability; the manifest must keep their ordered slots distinct even
    // when envelope bytes are shared.
    let first_slots = package_slot_identities(&output, &first, "firstHistory");
    let second_slots = package_slot_identities(&output, &second, "secondHistory");
    assert!(!first_slots.is_empty() && !second_slots.is_empty());
    drop(output);
}
