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
    assert_completed_output_keeps_actual_producer_with_private_equal_offer(
        &fixture,
        supports[0],
        offers[0],
        owner,
    );
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
    assert_interface_extension_preserves_native_topology(
        &checked_inputs[0][1],
        supports[1],
        owner,
        &fixture.root.descriptor.owner,
    );
    assert_fresh_dependency_permission_boundary(
        contexts[0].as_ref(),
        &checked_inputs[0][0],
        &checked_inputs[0][1],
        acquisitions[0],
    );
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
            InputNativeDecision::IssuedChoices(expected_groups),
            InputNativeDecision::IssuedChoices(actual_groups),
        ) => {
            assert_eq!(actual_groups, expected_groups);
            for group in expected_groups.values() {
                let expected_support = expected
                    .compiler_roles()
                    .selected_native_support(owner, *group)
                    .expect("the issuing selected row records its native support");
                let actual_support = actual
                    .compiler_roles()
                    .selected_native_support(owner, *group)
                    .expect("the chosen row retains native support with its native decision");
                assert_eq!(
                    matches!(expected_support, SelectedNativeSupport::Missing(_)),
                    matches!(actual_support, SelectedNativeSupport::Missing(_)),
                    "missing native permission cannot be filled by the losing row"
                );
                let expected_offers = expected_support.acquisitions();
                let actual_offers = actual_support.acquisitions();
                assert_eq!(actual_offers.len(), expected_offers.len());
                assert!(
                    actual_offers
                        .iter()
                        .zip(expected_offers)
                        .all(|(actual, expected)| actual.same_issuance(expected)),
                    "equal native facts cannot replace the selected actual contribution"
                );
            }
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

fn assert_fresh_dependency_permission_boundary(
    completed: &crate::declaration_context::ExactDeclarationContext,
    empty: &CompilerSourceSupport,
    full: &CompilerSourceSupport,
    helper: &Arc<ArtifactEntry>,
) {
    let owner = &helper.descriptor.owner;
    assert!(
        full.native_offers()
            .iter()
            .any(|offer| offer.original() == helper.descriptor.id && offer.is_full()),
        "the actual checked issuer must supply an individual Full helper offer"
    );
    let available = empty
        .retain_custody(full.artifacts())
        .unwrap()
        .retain_normalized_availability(full.native_availability())
        .unwrap()
        .retain_offers(full.native_offers().iter().cloned())
        .unwrap();
    let InputNativeDecision::IssuedChoices(choices) =
        available.compiler_roles().native_decision(owner)
    else {
        panic!("the selected input explicitly issues no root choices")
    };
    assert!(choices.is_empty());
    let ArtifactPayload::Original(product) = &helper.payload else {
        unreachable!()
    };
    let ordinal = helper
        .native_group_ordinals
        .iter()
        .copied()
        .find(|ordinal| {
            crate::certified_products::original_native_group_definitions(product, *ordinal)
                .unwrap()
                .export_contracts()
                .any(|contract| contract.unwrap().identity.occurrence == "second")
        })
        .expect("the actual Haskell helper exports the demanded second body");
    let key = NativeGroupKey {
        artifact: helper.descriptor.id,
        original_ordinal: ordinal,
    };
    let InputNativeDecision::IssuedChoices(full_choices) =
        full.compiler_roles().native_decision(owner)
    else {
        panic!("actual Full selected ingress carries exact group choices")
    };
    let expected_group = full_choices[&ordinal];
    let context = crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_issued_compiler_support(&available)
        .unwrap();
    let request = context
        .capture_world_primary_inputs(helper.descriptor.producer_sha256)
        .unwrap();
    let validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
    let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        request.inputs(),
        &validation.inventory,
    )
    .unwrap();
    assert!(
        matches!(
            super::super::native_availability::verified_root_choice(&selection, key, product.owner(), helper),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.native_refusal, Some(NativeAdmissionRefusal::RootChoiceExplicitEmpty {
                    original_ordinal, .. }) if original_ordinal == ordinal)
        ),
        "Full request-owned availability cannot override an explicit empty direct root decision"
    );
    completed.assert_fresh_dependency_acquires_available_group(
        &available,
        helper,
        expected_group,
        true,
    );

    // Restore actual recorded choices through the production authentication
    // owner. Decoding authentic graph facts cannot restore their live grant.
    let facts = full.native_fact_owner();
    let missing = full
        .restore_native_maps(
            full.recorded_rows(),
            &facts.offers,
            &facts.native_availability,
        )
        .unwrap()
        .retain_normalized_availability(full.native_availability())
        .unwrap()
        .retain_offers(full.native_offers().iter().cloned())
        .unwrap();
    assert!(matches!(
        missing
            .compiler_roles()
            .selected_native_support(owner, expected_group),
        Some(SelectedNativeSupport::Missing(_))
    ));
    completed.assert_fresh_dependency_acquires_available_group(
        &missing,
        helper,
        expected_group,
        false,
    );
}

fn assert_completed_output_keeps_actual_producer_with_private_equal_offer(
    fixture: &crate::artifacts::test_support::SourcePublicationEntryFixture,
    prior: &CompilerSourceSupport,
    prior_offer: &IssuedNativeOffer,
    owner: &ExactModuleIdentity,
) {
    let types = prior
        .selected_interfaces()
        .unwrap()
        .type_interfaces_for_owners(vec![owner.clone()])
        .unwrap()
        .retain_custody(prior.artifacts())
        .unwrap()
        .retain_normalized_availability(prior.native_availability())
        .unwrap()
        .retain_offers([prior_offer.clone()])
        .unwrap();
    assert!(types.compiler_roles().original_acquisition(owner).is_none());
    assert!(types
        .native_offers()
        .iter()
        .any(|offer| offer.same_issuance(prior_offer)));
    let context = crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_issued_compiler_support(&types)
        .unwrap();
    assert!(
        context.lexical_graph().is_empty(),
        "private A grants no authored source reuse"
    );
    let source = std::fs::read_to_string(fixture.includes[0].join("PublicationTarget.hs")).unwrap();
    let compiled = crate::artifacts::test_support::compile_invocation_in_context(
        &crate::artifacts::CompileInvocation {
            source: &source,
            targets: &["__prepared"],
            include: &fixture.includes,
            fallback_module_name: "PublicationTarget",
        },
        Arc::new(context),
        |_, _, _| {},
    )
    .expect("actual completed B is unambiguous beside equal-fact private A availability");
    let actual_product = compiled
        .recovery_products
        .iter()
        .find(|product| {
            product.owner().unit == owner.unit && product.owner().module == owner.module
        })
        .expect("TYPE input forces the actual compiler to produce B from source");
    let ArtifactPayload::Original(prior_product) = &prior_offer.original_acquisition().payload
    else {
        unreachable!()
    };
    assert!(actual_product.source_admission().is_some());
    assert!(
        !Arc::ptr_eq(
            actual_product.source_admission().unwrap(),
            prior_product.source_admission().unwrap()
        ),
        "the control must issue an independent actual completed source observation B"
    );
    assert_eq!(
        actual_product, prior_product,
        "B and private A have equal immutable facts"
    );
    let output = compiled
        .issued_output
        .as_ref()
        .expect("the actual compiler owns its completed output");
    assert!(output.source_selection().issued_native_availability()
        .flat_map(|row| row.offers()).any(|offer| offer.same_issuance(prior_offer)),
        "the completed selection must still retain actual A private availability as the crossed control");
    let own_offer = output
        .source_selection()
        .completed_compiler_output_offer(owner)
        .unwrap()
        .expect("the completed B receipt retains its own native offer");
    assert!(!own_offer.same_issuance(prior_offer));
    let support = CompilerSourceSupport::close_complete_output(output.compiler_inputs()).unwrap();
    let selected = support
        .compiler_roles()
        .original_acquisition(owner)
        .unwrap();
    let ArtifactPayload::Original(selected_product) = &selected.payload else {
        unreachable!()
    };
    assert!(
        selected_product.same_admission_and_transport(actual_product),
        "fresh row construction carries completed B's actual admission and transport"
    );
    assert!(
        Arc::ptr_eq(selected, own_offer.original_acquisition()),
        "fresh row construction carries B's exact own-offer acquisition"
    );
    if let InputNativeDecision::IssuedChoices(groups) =
        support.compiler_roles().native_decision(owner)
    {
        for group in groups.values() {
            let selected = support
                .compiler_roles()
                .selected_native_support(owner, *group)
                .unwrap();
            assert!(selected
                .acquisitions()
                .iter()
                .any(|offer| offer.same_issuance(own_offer)));
            assert!(
                !selected
                    .acquisitions()
                    .iter()
                    .any(|offer| offer.same_issuance(prior_offer)),
                "private equal-fact A cannot contribute to B's selected native decision"
            );
        }
    }
}

fn assert_interface_extension_preserves_native_topology(
    original: &CompilerSourceSupport,
    incoming: &CompilerSourceSupport,
    owner: &ExactModuleIdentity,
    added_owner: &ExactModuleIdentity,
) {
    let acquired_original = Arc::clone(
        original
            .compiler_roles()
            .original_acquisition(owner)
            .unwrap(),
    );
    let groups = original.artifacts().selected_native_groups();
    assert!(
        !groups.is_empty(),
        "the actual helper issuer selects executable group roots"
    );
    let receiver = original.for_source_owners(&BTreeSet::new()).unwrap();
    assert_eq!(
        receiver.artifacts().selected_native_groups(),
        groups,
        "compiler TYPE policy does not erase separately issued native graph demand"
    );
    assert!(receiver
        .compiler_roles()
        .original_acquisition(owner)
        .is_none());
    let before = receiver.artifacts().capture_graph_selection();
    let native_roots = |graph: &ArtifactGraphSelection| {
        graph
            .roots
            .iter()
            .copied()
            .filter(|node| matches!(node, ArtifactBindingNode::Group(_)))
            .collect::<BTreeSet<_>>()
    };
    assert!(!native_roots(&before).is_empty());
    let native_edges = |support: &CompilerSourceSupport| {
        support
            .artifacts()
            .binding_dependencies()
            .into_iter()
            .filter(|(from, _, _)| matches!(from, ArtifactBindingNode::Group(_)))
            .collect::<BTreeSet<_>>()
    };
    let edges = native_edges(&receiver);
    let canonical = receiver.selected[owner].interface().clone();
    let recorded = receiver.recorded_rows()[owner].clone();
    assert!(
        recorded.native_choices().is_none(),
        "a TYPE row has no selected native decision"
    );
    let addition = incoming
        .selected_interfaces()
        .unwrap()
        .type_interfaces_for_owners(vec![added_owner.clone()])
        .unwrap();
    assert!(receiver.compiler_roles().get(added_owner).is_none());
    let expected_addition = addition.selected[added_owner].interface().clone();
    let extended = receiver
        .extend_interfaces(&addition.selected_interfaces().unwrap())
        .unwrap();
    assert!(
        extended.compiler_roles().get(added_owner).is_some(),
        "the actual TYPE loan is installed"
    );
    assert_eq!(extended.artifacts().selected_native_groups(), groups);
    let after = extended.artifacts().capture_graph_selection();
    assert_eq!(native_roots(&after), native_roots(&before));
    assert_eq!(after.native_groups, before.native_groups);
    assert_eq!(
        native_edges(&extended),
        edges,
        "adding TYPE evidence preserves every exact executable group/carrier/dependency edge"
    );
    assert_eq!(extended.selected[owner].interface().0, canonical.0);
    assert!(Arc::ptr_eq(
        &extended.selected[owner].interface().1,
        &canonical.1
    ));
    assert_eq!(
        extended.selected[added_owner].interface().0,
        expected_addition.0
    );
    assert!(Arc::ptr_eq(
        &extended.selected[added_owner].interface().1,
        &expected_addition.1
    ));
    assert_eq!(extended.recorded_rows()[owner], recorded);
    assert!(matches!(
        extended.compiler_roles().native_decision(owner),
        InputNativeDecision::UnissuedPolicy
    ));
    assert!(extended
        .compiler_roles()
        .original_acquisition(owner)
        .is_none());
    let retained = extended
        .artifacts()
        .lease
        .payloads
        .get(&acquired_original.descriptor.id)
        .unwrap();
    assert!(retained.same_issuing_authority(&acquired_original),
        "the executable graph retains the actual original transport despite TYPE-only compiler policy");
}
