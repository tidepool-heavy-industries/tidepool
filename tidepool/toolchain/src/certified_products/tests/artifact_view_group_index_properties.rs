use super::*;
use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory, ArtifactPayload, ArtifactView};
use crate::declaration_context::certify_artifact_view_groups_with_validation;
use crate::CompileError;

fn context_failure(message: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("exact declaration context: {message}"))
}

// This is the pre-index wrapper: independently search full owners and the
// ordered candidate list, sharing only the downstream certification boundary.
fn linear_group_admission(
    view: &ArtifactView,
    candidates: &[PendingCertifiedGroup],
    baseline: &[PendingCertifiedGroup],
) -> Result<Vec<PendingCertifiedGroup>, CompileError> {
    let metadata = view.metadata_snapshot();
    let available = metadata
        .artifacts
        .iter()
        .filter_map(|(id, entry)| match &entry.payload {
            ArtifactPayload::Original(product) => Some((*id, product)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let selected_key = |group: &PendingCertifiedGroup| {
        let mut exact = available
            .iter()
            .filter(|(_, product)| product.owner() == group.owner());
        let (artifact, _) = exact
            .next()
            .ok_or_else(|| context_failure("certified group has no exact original artifact"))?;
        if exact.next().is_some() {
            return Err(context_failure(
                "certified group has ambiguous exact original artifacts",
            ));
        }
        Ok(crate::artifact_inventory::NativeGroupKey {
            artifact: *artifact,
            original_ordinal: group.group().original_ordinal(),
        })
    };
    let mut current = Vec::new();
    for group in candidates {
        if metadata
            .selected_native_groups
            .contains(&selected_key(group)?)
        {
            current.push(group.clone());
        }
    }
    let mut baseline_keys = BTreeSet::new();
    for group in baseline {
        let key = selected_key(group)?;
        if !baseline_keys.insert(key) || !metadata.selected_native_groups.contains(&key) {
            return Err(context_failure(
                "artifact view removed or duplicated a previously selected group",
            ));
        }
        if let Some(candidate) = current.iter().find(|candidate| {
            candidate.owner() == group.owner()
                && candidate.group().original_ordinal() == group.group().original_ordinal()
        }) {
            if candidate.group() != group.group() || candidate.imports() != group.imports() {
                return Err(context_failure(
                    "artifact view changed a previously selected group",
                ));
            }
        } else {
            current.push(group.clone());
        }
    }
    let additional = certify_selected_owned_products_in_context_with_validation(
        &available,
        &current,
        &metadata.selected_native_groups,
        &mut PackageInterfaceValidation::default(),
    )
    .map_err(context_failure)?;
    current.extend(additional);
    Ok(current)
}

fn compare_admission(
    view: &ArtifactView,
    candidates: &[PendingCertifiedGroup],
    baseline: &[PendingCertifiedGroup],
) {
    let before = (view.artifact_ids(), view.selected_native_groups());
    let expected = linear_group_admission(view, candidates, baseline);
    let actual = certify_artifact_view_groups_with_validation(
        view,
        candidates,
        baseline,
        &mut PackageInterfaceValidation::default(),
    );
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => {
            assert_eq!(actual, expected);
            for (actual, expected) in actual.iter().zip(&expected) {
                assert!(std::ptr::eq(actual.group(), expected.group()));
                assert!(std::ptr::eq(actual.imports(), expected.imports()));
            }
        }
        (Err(CompileError::ExtractFailed(actual)), Err(CompileError::ExtractFailed(expected))) => {
            assert_eq!(
                actual, expected,
                "refusal variant and precedence are preserved"
            );
        }
        (actual, expected) => {
            panic!("different admission outcomes: {actual:?} versus {expected:?}")
        }
    }
    assert_eq!((view.artifact_ids(), view.selected_native_groups()), before);
}

fn changed_imports(group: &PendingCertifiedGroup) -> PendingCertifiedGroup {
    let mut changed = group.clone();
    changed.imports = vec![PendingImportOwner::Retained {
        identity: testing::identity("Captured", "different"),
        generation: 99,
    }]
    .into();
    changed
}

fn changed_body(group: &PendingCertifiedGroup) -> PendingCertifiedGroup {
    let mut changed = group.clone();
    changed.group = Arc::new(
        testing::projected_group(testing::wire_program(), group.group().original_ordinal())
            .unwrap(),
    );
    changed
}

fn check_admission_shapes(ordinals: &[u32], choices: &[(u8, u8, u8)]) {
    let products = ["Shared", "Shared", "Other"]
        .into_iter()
        .enumerate()
        .map(|(index, module)| {
            full_native_fixture(
                module,
                ordinals.iter().map(|ordinal| (*ordinal, vec![])).collect(),
                7 + index as u8,
            )
        })
        .collect::<Vec<_>>();
    let groups = products
        .iter()
        .flat_map(|product| {
            certify_owned_products_with_validation(
                &[product],
                &[],
                &mut PackageInterfaceValidation::default(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let entries = products
        .iter()
        .cloned()
        .map(|product| Arc::new(ArtifactEntry::original([1; 32], product).unwrap()))
        .collect::<Vec<_>>();
    let mut selected = groups
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            *index == 0 || choices.get(*index).is_some_and(|choice| choice.0 & 1 != 0)
        })
        .map(|(_, group)| {
            let artifact = entries
                .iter()
                .find(|entry| match &entry.payload {
                    ArtifactPayload::Original(product) => product.owner() == group.owner(),
                    _ => false,
                })
                .unwrap()
                .descriptor
                .id;
            crate::artifact_inventory::NativeGroupKey {
                artifact,
                original_ordinal: group.group().original_ordinal(),
            }
        })
        .collect::<BTreeSet<_>>();
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_recovery_selection(&inventory.empty_view(), entries.clone(), &selected)
        .unwrap();
    let mut candidates = groups
        .iter()
        .enumerate()
        .filter(|(index, _)| choices.get(*index).is_some_and(|choice| choice.0 & 2 != 0))
        .map(|(index, group)| (choices[index].1, group.clone()))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(rank, _)| *rank);
    let candidates = candidates
        .into_iter()
        .map(|(_, group)| group)
        .collect::<Vec<_>>();
    let selected_groups = groups
        .iter()
        .filter(|group| {
            entries.iter().any(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => {
                    product.owner() == group.owner()
                        && selected.contains(&crate::artifact_inventory::NativeGroupKey {
                            artifact: entry.descriptor.id,
                            original_ordinal: group.group().original_ordinal(),
                        })
                }
                _ => false,
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut baseline = selected_groups
        .iter()
        .enumerate()
        .filter(|(index, _)| choices.get(*index).is_some_and(|choice| choice.0 & 4 != 0))
        .map(|(index, group)| (choices[index].2, group.clone()))
        .collect::<Vec<_>>();
    baseline.sort_by_key(|(rank, _)| *rank);
    let baseline = baseline
        .into_iter()
        .map(|(_, group)| group)
        .collect::<Vec<_>>();
    compare_admission(&view, &candidates, &baseline);
    compare_admission(
        &view,
        &[],
        &selected_groups.iter().rev().cloned().collect::<Vec<_>>(),
    );
    let Some(first) = selected_groups.first() else {
        return;
    };

    let duplicate = vec![first.clone(), first.clone()];
    compare_admission(&view, &duplicate, std::slice::from_ref(first));
    compare_admission(&view, &[], &duplicate);
    let mut wrong_owner = first.clone();
    wrong_owner.owner.module_version = ModuleVersion([250; 32]);
    compare_admission(&view, std::slice::from_ref(&wrong_owner), &[]);
    compare_admission(&view, &[], std::slice::from_ref(&wrong_owner));
    for changed in [changed_imports(first), changed_body(first)] {
        compare_admission(
            &view,
            std::slice::from_ref(&changed),
            std::slice::from_ref(first),
        );
        compare_admission(
            &view,
            std::slice::from_ref(first),
            std::slice::from_ref(&changed),
        );
        // Selecting the last duplicate rather than the first changes which
        // boundary refuses the input, even though both refuse it.
        compare_admission(
            &view,
            &[first.clone(), changed.clone()],
            std::slice::from_ref(first),
        );
        compare_admission(
            &view,
            &[changed, first.clone()],
            std::slice::from_ref(first),
        );
    }
    let removed = selected.pop_first().unwrap();
    let removed_view = inventory
        .admit_recovery_selection(&inventory.empty_view(), entries, &selected)
        .unwrap();
    let removed_group = selected_groups
        .iter()
        .find(|group| {
            group.group().original_ordinal() == removed.original_ordinal
                && match &view.metadata_snapshot().artifacts[&removed.artifact].payload {
                    ArtifactPayload::Original(product) => product.owner() == group.owner(),
                    _ => false,
                }
        })
        .unwrap();
    compare_admission(
        &removed_view,
        &candidates,
        std::slice::from_ref(removed_group),
    );
}

#[test]
fn artifact_view_group_indexes_preserve_order_and_refusal_precedence() {
    check_admission_shapes(
        &[u32::MAX, 0, 29],
        &[(7, 9, 2), (1, 8, 3), (7, 0, 9), (7, 1, 0)],
    );
    check_admission_shapes(&[], &[]);
    check_admission_shapes(&[7], &[(7, 0, 0); 3]);
}

proptest::proptest! {
    #![proptest_config(original_native_index_property_config())]
    #[test]
    fn artifact_view_group_indexes_match_linear_wrapper_for_generated_selections(
        ordinals in proptest::collection::vec(proptest::prelude::any::<u32>(), 0..12),
        choices in proptest::collection::vec((proptest::prelude::any::<u8>(),
            proptest::prelude::any::<u8>(), proptest::prelude::any::<u8>()), 0..36),
    ) {
        let mut seen = BTreeSet::new();
        let ordinals = ordinals.into_iter().filter(|ordinal| seen.insert(*ordinal)).collect::<Vec<_>>();
        check_admission_shapes(&ordinals, &choices);
    }
}
