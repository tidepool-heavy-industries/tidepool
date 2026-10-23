//! Equal immutable Originals retain their actual completed source acquisition.

use super::*;

#[test]
fn authored_promotion_preserves_actual_offered_acquisition() {
    let fixture = crate::artifacts::test_support::prepare_source_publication_entry_fixture()
        .expect("the actual compiler issues the first completed source observation");
    let frozen = crate::artifacts::FrozenEntrySources::capture(
        &fixture.includes,
        &fixture.includes[0].join("PublicationTarget.hs"),
    )
    .unwrap();
    let second_directory = crate::artifacts::test_support::compiler_input_directory().unwrap();
    let second_entry = crate::artifacts::test_support::prepare_frozen_production_entry(
        &frozen,
        second_directory.path(),
        &second_directory.path().join("entry"),
    )
    .expect("an independent completed request issues the second source observation");
    let contexts = [
        fixture
            .entry
            .products()
            .original_execution_context()
            .unwrap(),
        second_entry
            .products()
            .original_execution_context()
            .unwrap(),
    ];
    let supports = contexts
        .each_ref()
        .map(|context| context.compiler_source_support());
    let owner = &fixture.helper.descriptor.owner;
    let original = fixture.helper.descriptor.id;
    let acquisitions = supports.map(|support| {
        support
            .compiler_roles()
            .original_acquisition(owner)
            .expect("the completed cohort selects the actual helper Original")
    });
    let observation = |entry: &ArtifactEntry| {
        let ArtifactPayload::Original(product) = &entry.payload else {
            panic!("the acquired helper is an Original");
        };
        Arc::clone(
            product
                .source_admission()
                .expect("a completed source observation"),
        )
    };
    let observations = acquisitions.map(|entry| observation(entry));
    assert_eq!(acquisitions[0].descriptor.id, original);
    assert_eq!(acquisitions[1].descriptor.id, original);
    assert_eq!(acquisitions[0], acquisitions[1], "immutable facts agree");
    assert!(
        !Arc::ptr_eq(&observations[0], &observations[1]),
        "independent completed observations must exercise different actual owners"
    );
    assert!(!acquisitions[0].same_issuing_authority(acquisitions[1]));
    assert_eq!(
        supports[0].selected[owner].interface().0,
        supports[1].selected[owner].interface().0,
        "equal canonical bindings allow either real offer to qualify for this TYPE row"
    );
    let offers = supports.map(|support| {
        support
            .native_offers()
            .iter()
            .find(|offer| offer.original() == original)
            .expect("the completed cohort carries its helper's actual native offer")
    });
    assert!(!offers[0].same_issuance(offers[1]));
    for index in 0..2 {
        assert!(Arc::ptr_eq(
            &observation(offers[index].original_acquisition()),
            &observations[index]
        ));
    }
    let owners = BTreeSet::from([owner.clone()]);
    let checked_inputs = acquisitions.map(|entry| {
        [
            NativeArtifactDemand::ScopeInterfaces,
            NativeArtifactDemand::AllGroups,
        ]
        .map(|demand| {
            let inventory = ArtifactInventory::default();
            let entries = vec![Arc::clone(entry)];
            CompilerSourceSupport::from_selected_issuance(
                CompilerInputProjection::from_issued_entries(&entries).unwrap(),
                inventory
                    .admit_shared_with_demand(&inventory.empty_view(), entries, demand)
                    .unwrap(),
            )
            .unwrap()
        })
    });
    for issued in &checked_inputs {
        for (nonempty, support) in issued.iter().enumerate() {
            let expected = support
                .compiler_roles()
                .original_acquisition(owner)
                .unwrap();
            let ArtifactPayload::Original(product) = &expected.payload else {
                unreachable!()
            };
            let canonical = support.compiler_roles().get(owner).unwrap().interface();
            let decision = support
                .checked_native_input_decision(original, canonical, product.owner())
                .unwrap();
            let InputNativeDecision::IssuedChoices(groups) = decision else {
                panic!("the checked input issuer records explicit native choices");
            };
            assert_eq!(groups.is_empty(), nonempty == 0,
                "actual ScopeInterfaces and AllGroups ingress distinguish issued-empty from nonempty");
            let mut wrong_owner = product.owner().clone();
            wrong_owner.product_sha256[0] ^= 1;
            assert!(matches!(
                support.checked_native_input_decision(original, canonical, &wrong_owner),
                Err(crate::certified_products::InputDecisionRefusal::NativeOwnerMismatch),
            ));
            assert!(matches!(
                support.checked_native_input_decision(
                    fixture.root.descriptor.id,
                    canonical,
                    product.owner()
                ),
                Err(crate::certified_products::InputDecisionRefusal::ArtifactIdentityMismatch(_)),
            ));
            assert_narrowing_keeps_selected_acquisition(support, owner, expected);
        }
        assert_whole_row_precedence(&issued[0], &issued[1], owner);
    }
    for (first, second) in [
        (&checked_inputs[0], &checked_inputs[1]),
        (&checked_inputs[1], &checked_inputs[0]),
    ] {
        assert_whole_row_precedence(&first[0], &second[1], owner);
    }
    for offered in 0..2 {
        let types = supports[offered]
            .selected_interfaces()
            .unwrap()
            .type_interfaces_for_owners(vec![owner.clone()])
            .unwrap();
        assert!(types.compiler_roles().original_acquisition(owner).is_none());
        assert!(
            !types.artifacts().lease.payloads.contains_key(&original),
            "the TYPE projection has no prior Original acquisition to mask the control"
        );
        for retained in [offered, 1 - offered] {
            // Custody and normalized availability have their own actual owner.
            // They do not choose the acquisition selected by this carried offer.
            let source = types
                .retain_custody(supports[retained].artifacts())
                .unwrap()
                .retain_normalized_availability(supports[retained].native_availability())
                .unwrap()
                .retain_offers([offers[offered].clone()])
                .unwrap();
            let row = source
                .native_availability()
                .issued_row(original)
                .expect("the real retained availability row survives TYPE narrowing");
            let available = row.target_original(row.original_owner()).unwrap();
            assert!(Arc::ptr_eq(
                &observation(available),
                &observations[retained]
            ));
            let custody = source.artifacts().lease.payloads.get(&original).unwrap();
            assert!(
                Arc::ptr_eq(&observation(custody), &observations[retained]),
                "the mismatched control must actually retain the other acquisition"
            );
            assert!(source
                .compiler_roles()
                .original_acquisition(owner)
                .is_none());
            let promoted = source
                .promote_issued_authored_originals(supports[offered], &owners)
                .expect("the one actual carried offer qualifies for authored promotion");
            let selected = promoted
                .compiler_roles()
                .original_acquisition(owner)
                .unwrap();
            assert!(
                Arc::ptr_eq(selected, offers[offered].original_acquisition()),
                "promotion carries the offer's exact acquired entry"
            );
            assert!(
                Arc::ptr_eq(&observation(selected), &observations[offered]),
                "an equal payload in broad availability cannot replace the issuing observation"
            );
            let products = if offered == 0 {
                fixture.entry.products()
            } else {
                second_entry.products()
            };
            contexts[offered].assert_selected_acquisition_publication_history(
                &promoted,
                supports[retained],
                owner,
                offers[offered].original_acquisition(),
                products
                    .original_compile_input
                    .as_ref()
                    .unwrap()
                    .original_input_identity(),
                products.source_selection(),
            );
            assert_narrowing_keeps_selected_acquisition(&promoted, owner, selected);
            assert_whole_row_precedence(&promoted, supports[retained], owner);
            let repeated = source
                .retain_offers([offers[offered].clone(), offers[offered].clone()])
                .unwrap()
                .promote_issued_authored_originals(supports[offered], &owners)
                .unwrap();
            assert!(
                Arc::ptr_eq(
                    repeated
                        .compiler_roles()
                        .original_acquisition(owner)
                        .unwrap(),
                    selected
                ),
                "repeating the same actual offer preserves its acquisition"
            );
            for order in [[offered, 1 - offered], [1 - offered, offered]] {
                let ambiguous = source
                    .retain_offers(order.map(|index| offers[index].clone()))
                    .unwrap();
                let before = ambiguous.artifacts().capture_graph_selection();
                assert!(
                    matches!(ambiguous.promote_issued_authored_originals(supports[offered], &owners),
                    Err(CompileError::ArtifactInventory(error))
                        if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                            if artifact == original)),
                    "two qualifying completed source acquisitions must refuse in either order"
                );
                assert!(ambiguous
                    .compiler_roles()
                    .original_acquisition(owner)
                    .is_none());
                assert_eq!(ambiguous.artifacts().capture_graph_selection(), before);
            }
        }
    }
}

// The oracle is the pre-transform acquired entry, not a reconstruction of the
// narrowed role from its retained artifact inventory.
fn assert_narrowing_keeps_selected_acquisition(
    source: &CompilerSourceSupport,
    owner: &ExactModuleIdentity,
    expected: &Arc<ArtifactEntry>,
) {
    let original = source
        .for_source_owners(&BTreeSet::from([owner.clone()]))
        .unwrap();
    assert!(Arc::ptr_eq(
        original
            .compiler_roles()
            .original_acquisition(owner)
            .unwrap(),
        expected,
    ));
    assert_same_native_decision(source, &original, owner);
    let types = source.for_source_owners(&BTreeSet::new()).unwrap();
    assert!(types.compiler_roles().original_acquisition(owner).is_none());
    assert!(matches!(
        types.compiler_roles().native_decision(owner),
        InputNativeDecision::UnissuedPolicy,
    ));
}

fn assert_same_native_decision(
    expected: &CompilerSourceSupport,
    actual: &CompilerSourceSupport,
    owner: &ExactModuleIdentity,
) {
    match (
        expected.compiler_roles().native_decision(owner),
        actual.compiler_roles().native_decision(owner),
    ) {
        (InputNativeDecision::UnissuedPolicy, InputNativeDecision::UnissuedPolicy) => {}
        (
            InputNativeDecision::IssuedChoices(expected),
            InputNativeDecision::IssuedChoices(actual),
        ) => {
            assert_eq!(actual, expected);
        }
        _ => panic!("a whole-row operation changed whether a native choice was issued"),
    }
}

fn assert_whole_row_precedence(
    first: &CompilerSourceSupport,
    second: &CompilerSourceSupport,
    owner: &ExactModuleIdentity,
) {
    for (receiving, incoming) in [(first, second), (second, first)] {
        // Equal Original policy chooses Receiving, including when the other
        // completed acquisition was retained first as transport custody.
        let receiving = receiving.retain_custody(incoming.artifacts()).unwrap();
        let expected = receiving
            .compiler_roles()
            .original_acquisition(owner)
            .unwrap();
        let appended = receiving.append_program_fragment(incoming).unwrap().support;
        assert!(Arc::ptr_eq(
            appended
                .compiler_roles()
                .original_acquisition(owner)
                .unwrap(),
            expected,
        ));
        assert_same_native_decision(&receiving, &appended, owner);
        // A TYPE receiver chooses the complete Incoming Original row.
        let types = receiving.for_source_owners(&BTreeSet::new()).unwrap();
        let appended = types.append_program_fragment(incoming).unwrap().support;
        assert!(Arc::ptr_eq(
            appended
                .compiler_roles()
                .original_acquisition(owner)
                .unwrap(),
            incoming
                .compiler_roles()
                .original_acquisition(owner)
                .unwrap(),
        ));
        assert_same_native_decision(incoming, &appended, owner);
    }
}
