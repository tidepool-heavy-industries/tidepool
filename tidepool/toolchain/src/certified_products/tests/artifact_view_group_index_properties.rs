use super::*;
use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory, NativeGroupKey};

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
    let selected = groups.iter().enumerate().filter(|(index, _)| *index == 0 || choices.get(*index).is_some_and(|choice| choice.0 & 1 != 0))
        .map(|(_, group)| NativeGroupKey {
            artifact: entries.iter().find(|entry| matches!(&entry.payload, crate::artifact_inventory::ArtifactPayload::Original(product) if product.owner() == group.owner())).unwrap().descriptor.id,
            original_ordinal: group.group().original_ordinal(),
        }).collect::<BTreeSet<_>>();
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
    let issued = IssuedGroupCandidates::bind(
        candidates
            .into_iter()
            .map(|(_, group)| nominal_pending_groups(&[group]).0.remove(0))
            .collect::<Vec<_>>()
            .into(),
        &products,
    )
    .unwrap();
    let before = (view.artifact_ids(), view.selected_native_groups());
    let admitted = crate::declaration_context::certify_issued_artifact_view_groups_with_validation(
        &view,
        &issued,
        None,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(admitted.len(), selected.len());
    // The independent physical oracle reads authenticated fixture facts. It
    // does not issue a namespace or infer scoped edges from these logical keys.
    for key in &selected {
        let product = products.iter().find(|product| entries.iter().any(|entry| entry.descriptor.id == key.artifact && matches!(&entry.payload, crate::artifact_inventory::ArtifactPayload::Original(p) if p.owner() == product.owner()))).unwrap();
        let expected = product
            .original_native()
            .unwrap()
            .group(key.original_ordinal)
            .unwrap();
        let actual = admitted
            .iter()
            .find(|group| {
                group.owner() == product.owner()
                    && group.group().original_ordinal() == key.original_ordinal
            })
            .unwrap();
        assert!(std::ptr::eq(actual.group(), expected.group()));
        assert_eq!(actual.imports(), expected.source_imports(product.original_native().unwrap()).unwrap());
    }
    let repeated =
        crate::declaration_context::certify_selected_artifact_view_groups_with_validation(
            &view,
            Some(&admitted),
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
    for binding in view.selected_native_binding_rows().1 {
        assert!(std::ptr::eq(
            admitted.get(binding).unwrap().group(),
            repeated.get(binding).unwrap().group()
        ));
    }
    assert_eq!((view.artifact_ids(), view.selected_native_groups()), before);
    let Some(first) = admitted.first() else {
        return;
    };
    let duplicated =
        IssuedGroupCandidates::bind(nominal_pending_groups(&[first.clone(), first.clone()]).0.into(), &products).unwrap();
    assert!(
        crate::declaration_context::certify_issued_artifact_view_groups_with_validation(
            &view,
            &duplicated,
            None,
            &mut PackageInterfaceValidation::default()
        )
        .is_err()
    );
    let mut foreign = first.clone();
    foreign.owner.module_version = ModuleVersion([250; 32]);
    assert!(IssuedGroupCandidates::bind(nominal_pending_groups(&[foreign]).0.into(), &products).is_err());
    let mut body = first.clone();
    body.group = Arc::new(
        testing::projected_group(testing::wire_program(), first.group().original_ordinal())
            .unwrap(),
    );
    let mut imports = first.clone();
    imports.imports = vec![PendingImportOwner::Retained {
        identity: testing::identity("Captured", "different"),
        generation: 99,
    }]
    .into();
    for changed in [body, imports] {
        let issued = IssuedGroupCandidates::bind(nominal_pending_groups(&[changed]).0.into(), &products).unwrap();
        assert!(
            crate::declaration_context::certify_issued_artifact_view_groups_with_validation(
                &view,
                &issued,
                Some(&admitted),
                &mut PackageInterfaceValidation::default()
            )
            .is_err()
        );
    }
    let mut removed = selected.clone();
    removed.pop_first();
    let narrowed = inventory
        .admit_recovery_selection(&inventory.empty_view(), entries, &removed)
        .unwrap();
    assert!(
        crate::declaration_context::certify_selected_artifact_view_groups_with_validation(
            &narrowed,
            Some(&admitted),
            &mut PackageInterfaceValidation::default()
        )
        .is_err()
    );
}

#[test]
fn artifact_view_group_indexes_preserve_physical_identity_and_exact_refusals() {
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
    fn artifact_view_group_indexes_match_physical_oracle_for_generated_selections(
        ordinals in proptest::collection::vec(proptest::prelude::any::<u32>(), 0..12),
        choices in proptest::collection::vec((proptest::prelude::any::<u8>(), proptest::prelude::any::<u8>(), proptest::prelude::any::<u8>()), 0..36),
    ) {
        let mut seen = BTreeSet::new();
        let ordinals = ordinals.into_iter().filter(|ordinal| seen.insert(*ordinal)).collect::<Vec<_>>();
        check_admission_shapes(&ordinals, &choices);
    }
}
