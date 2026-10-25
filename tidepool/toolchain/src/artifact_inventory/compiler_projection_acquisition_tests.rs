//! Equal immutable Originals retain their actual completed source acquisition.

use super::*;

#[test]
fn authored_promotion_preserves_actual_offered_acquisition() {
    let offer_operation = std::sync::Arc::new(
        tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
    );
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
                .expect("a completed source observation")
                .evidence(),
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
    let native_witnesses = acquisitions.map(|entry| {
        let ArtifactPayload::Original(product) = &entry.payload else {
            unreachable!()
        };
        product.original_native()
    });
    assert!(!Arc::ptr_eq(native_witnesses[0], native_witnesses[1]));
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
            let mut offer_validation =
                crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(Arc::clone(
                    &offer_operation,
                ));
            let entries = vec![Arc::clone(entry)];
            CompilerSourceSupport::from_selected_issuance(
                CompilerInputProjection::from_issued_entries(&entries).unwrap(),
                inventory
                    .admit_shared_with_demand(&inventory.empty_view(), entries, demand)
                    .unwrap(),
                &mut offer_validation,
            )
            .unwrap()
        })
    });
    assert_eq!(
        checked_inputs[0][1]
            .artifacts()
            .selected_native_binding_rows()
            .1,
        checked_inputs[1][1]
            .artifacts()
            .selected_native_binding_rows()
            .1,
        "equal graph bindings can carry independently allocated native witnesses"
    );
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
        assert_reconciled_whole_row_precedence(&issued[0], &issued[1], owner);
    }
    for (first, second) in [
        (&checked_inputs[0], &checked_inputs[1]),
        (&checked_inputs[1], &checked_inputs[0]),
    ] {
        assert_reconciled_whole_row_precedence(&first[0], &second[1], owner);
    }
    let shared_inputs = acquisitions.map(|entry| {
        let inventory = ArtifactInventory::default();
        let mut offer_validation =
            crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(Arc::clone(
                &offer_operation,
            ));
        let entries = vec![Arc::clone(entry)];
        let base = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries.clone(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let groups = entry
            .native_group_ordinals
            .iter()
            .map(|ordinal| NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: *ordinal,
            })
            .collect();
        let full = inventory
            .admit_recovery_selection(&base, Vec::new(), &groups)
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        [base, full].map(|view| {
            CompilerSourceSupport::from_selected_issuance(
                projection.clone(),
                view,
                &mut offer_validation,
            )
            .unwrap()
        })
    });
    for inputs in &shared_inputs {
        assert_strict_append_whole_row_precedence(&inputs[0], &inputs[1], owner);
    }
    for (first, second) in [
        (&shared_inputs[0], &shared_inputs[1]),
        (&shared_inputs[1], &shared_inputs[0]),
    ] {
        assert_strict_append_whole_row_precedence(&first[0], &second[1], owner);
    }
    assert_interface_extension_preserves_native_topology(
        &checked_inputs[0][1],
        supports[1],
        owner,
        &fixture.root.descriptor.owner,
    );
    let dependency_publication = fixture
        .entry
        .products()
        .published_source_original_selection("dependency-source", owner)
        .unwrap();
    assert_fresh_dependency_permission_boundary(
        dependency_publication.context().as_ref(),
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
            assert_reconciled_whole_row_precedence(&promoted, supports[retained], owner);
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

fn assert_whole_selected_row(
    expected: &CompilerSourceSupport,
    actual: &CompilerSourceSupport,
    owner: &ExactModuleIdentity,
) {
    let expected_row = &expected.selected[owner];
    let actual_row = &actual.selected[owner];
    assert_eq!(actual_row.interface().0, expected_row.interface().0);
    assert!(Arc::ptr_eq(
        &actual_row.interface().1,
        &expected_row.interface().1
    ));
    match (expected_row.original(), actual_row.original()) {
        (Some((expected_binding, expected)), Some((actual_binding, actual))) => {
            assert_eq!(actual_binding, expected_binding);
            assert!(Arc::ptr_eq(actual, expected));
        }
        (None, None) => {}
        _ => panic!("the selected whole row changed its Original policy"),
    }
    match (expected_row.publication(), actual_row.publication()) {
        (Some(expected), Some(actual)) => assert!(Arc::ptr_eq(&actual.birth, &expected.birth)),
        (None, None) => {}
        _ => panic!("the selected whole row changed its publication provenance"),
    }
    assert_eq!(
        actual.recorded_rows()[owner],
        expected.recorded_rows()[owner]
    );
    assert_same_native_decision(expected, actual, owner);
}

fn assert_reconciled_whole_row_precedence(
    first: &CompilerSourceSupport,
    second: &CompilerSourceSupport,
    owner: &ExactModuleIdentity,
) {
    for (receiving, incoming) in [(first, second), (second, first)] {
        let receiving = receiving.retain_custody(incoming.artifacts()).unwrap();
        let receiving_row = &receiving.selected[owner];
        let incoming_row = &incoming.selected[owner];
        let different_pair = receiving_row.interface().0 != incoming_row.interface().0
            || receiving_row.original().map(|(binding, _)| binding)
                != incoming_row.original().map(|(binding, _)| binding);
        if different_pair {
            let reason = if receiving_row.interface().0 != incoming_row.interface().0 {
                ProgramExtensionRefusal::InterfaceBinding {
                    receiving: receiving_row.interface().0,
                    incoming: incoming_row.interface().0,
                }
            } else {
                ProgramExtensionRefusal::OriginalBinding {
                    receiving: receiving_row.original().unwrap().0,
                    incoming: incoming_row.original().unwrap().0,
                }
            };
            assert!(
                matches!(receiving.checked_program_extension(incoming),
                Err(CompileError::ArtifactInventory(error))
                    if matches!(&error.failure, ArtifactInventoryFailure::ProgramExtension {
                        owner: refused_owner, reason: refused_reason,
                    } if refused_owner == owner && refused_reason == &reason)),
                "independent scopes refuse strict append with the exact conflicting binding pair"
            );
        }
        // Independent reconciliation chooses the whole Receiving Original,
        // retaining the other issuer's exact graph only as custody.
        let joined = receiving.merge(incoming).unwrap();
        assert_whole_selected_row(&receiving, &joined, owner);
        let types = receiving.for_source_owners(&BTreeSet::new()).unwrap();
        let joined = types.merge(incoming).unwrap();
        assert_whole_selected_row(incoming, &joined, owner);
    }
}

fn assert_strict_append_whole_row_precedence(
    first: &CompilerSourceSupport,
    second: &CompilerSourceSupport,
    owner: &ExactModuleIdentity,
) {
    assert_eq!(
        first.selected[owner].interface().0,
        second.selected[owner].interface().0,
        "strict append controls share their actual admitted canonical binding"
    );
    assert_eq!(
        first.selected[owner].original().unwrap().0,
        second.selected[owner].original().unwrap().0,
        "adding executable group roots against one base preserves its Original carrier"
    );
    for (receiving, incoming) in [(first, second), (second, first)] {
        let receiving = receiving.retain_custody(incoming.artifacts()).unwrap();
        let appended = receiving
            .checked_program_extension(incoming)
            .unwrap()
            .apply()
            .unwrap()
            .support;
        assert_whole_selected_row(&receiving, &appended, owner);
        let types = receiving.for_source_owners(&BTreeSet::new()).unwrap();
        let appended = types
            .checked_program_extension(incoming)
            .unwrap()
            .apply()
            .unwrap()
            .support;
        assert_whole_selected_row(incoming, &appended, owner);
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
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
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
    // Source import authority and native dependency permission are independent.
    // Keep the actual publication for source compilation; reusable input-policy
    // variants below cannot replace its completed source authority.
    let parent = completed.assert_fresh_dependency_from_published_source(helper, ordinal);

    // Restore actual recorded choices through the production authentication
    // owner. Decoding authentic graph facts cannot restore their live grant.
    let facts = full.native_fact_owner();
    let missing = full
        .restore_native_maps(
            full.recorded_rows(),
            &facts.offers,
            &facts.native_availability,
            &mut validation,
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
    let expected_offers = full
        .native_offers()
        .iter()
        .filter(|offer| offer.is_full() && offer.contains_checked_group(expected_group))
        .collect::<Vec<_>>();
    assert_eq!(expected_offers.len(), 1);
    super::super::native_availability::assert_actual_emitted_dependency_policy(
        &available,
        &missing,
        &parent,
        helper,
        expected_group,
        expected_offers[0],
    );
}

#[test]
fn completed_output_keeps_actual_world_producer_beside_equal_canonical_private_offer() {
    let fixture = crate::artifacts::test_support::prepare_source_publication_entry_fixture()
        .expect("the actual compiler issues the prior completed source observation");
    let context = fixture
        .entry
        .products()
        .original_execution_context()
        .unwrap();
    let support = context.compiler_source_support();
    let owner = &fixture.helper.descriptor.owner;
    let offer = support
        .native_offers()
        .iter()
        .find(|offer| offer.original() == fixture.helper.descriptor.id)
        .expect("the completed cohort carries its helper's actual native offer");
    assert_completed_output_keeps_actual_producer_beside_equal_canonical_private_offer(
        &fixture, support, offer, owner,
    );
}

fn assert_completed_output_keeps_actual_producer_beside_equal_canonical_private_offer(
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
    let refused = crate::artifacts::test_support::compile_invocation_in_context(
        &crate::artifacts::CompileInvocation {
            source: &source,
            targets: &["__prepared"],
            include: &fixture.includes,
            fallback_module_name: "PublicationTarget",
        },
        Arc::new(context),
        |_, _, _| {},
    );
    let Err(CompileError::InputRejected(diagnostics)) = refused else {
        panic!("generic Exact TYPE input must not grant current authored SOURCE lookup");
    };
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message,
        "OriginalSourceSelectionRejected (ExecutionSourceUnavailable (\"main\",\"PublicationHelper\"))");
    assert_world_completed_output_keeps_actual_producer(fixture, &types, prior_offer, owner);
}

fn assert_world_completed_output_keeps_actual_producer(
    fixture: &crate::artifacts::test_support::SourcePublicationEntryFixture,
    private: &CompilerSourceSupport,
    prior_offer: &IssuedNativeOffer,
    owner: &ExactModuleIdentity,
) {
    use crate::cell_plan::{WorldCellAdmissionSubject, WorldCellReservationRequest};
    use crate::compiler_world::{
        CapturedCompilerSourceRoot, CapturedCompilerSources, CompilerWorldLexicalSurface,
        VisibleWorldBindings, WorldSourceSelection,
    };
    use tidepool_extract_cmd::{
        settle_compiler_transaction_cancellable, CompilerTransactionCancellation,
        CompilerTransactionClose, CompilerTransactionOrigin,
    };

    let prior_acquisition = Arc::clone(prior_offer.original_acquisition());
    let prior_graph = prior_offer
        .proof()
        .expect("actual A owns its checked native closure")
        .selection
        .graph_selection()
        .clone();
    assert!(!prior_graph.native_groups.is_empty());

    let root_owner = Arc::new(crate::artifacts::test_support::compiler_input_directory().unwrap());
    let helper_body = std::fs::read(fixture.includes[0].join("PublicationHelper.hs")).unwrap();
    let mut source_roots = vec![CapturedCompilerSourceRoot {
        manifest: crate::cache::SourceRootManifest::from_file_digests([(
            std::path::PathBuf::from("PublicationHelper.hs"),
            blake3::hash(&helper_body).to_hex().to_string(),
        )])
        .unwrap(),
        bodies: BTreeMap::from([(
            std::path::PathBuf::from("PublicationHelper.hs"),
            Arc::<[u8]>::from(helper_body),
        )]),
    }];
    let prelude = std::path::PathBuf::from(
        std::env::var_os("TIDEPOOL_PRELUDE_DIR")
            .expect("the declared source compiler owns its renderer source roots"),
    );
    let mut pending = vec![prelude.clone()];
    let mut bodies = BTreeMap::new();
    let mut remaining = CapturedCompilerSources::capture_byte_allowance().unwrap();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "hs") {
                let body = std::fs::read(&path).unwrap();
                remaining = remaining
                    .checked_sub(body.len() as u64)
                    .expect("declared source capture remains inside its owner allowance");
                bodies.insert(
                    path.strip_prefix(&prelude).unwrap().to_path_buf(),
                    Arc::<[u8]>::from(body),
                );
            }
        }
    }
    source_roots.push(CapturedCompilerSourceRoot {
        manifest: crate::cache::SourceRootManifest::from_file_digests(
            bodies
                .iter()
                .map(|(path, body)| (path.clone(), blake3::hash(body).to_hex().to_string())),
        )
        .unwrap(),
        bodies,
    });
    let sources = Arc::new(
        CapturedCompilerSources::from_captured_roots(
            source_roots,
            Arc::from(&b"actual-crossed-producer-world"[..]),
            root_owner,
        )
        .unwrap(),
    );
    let source =
        WorldSourceSelection::new(*sources.seal(), "actual-crossed-producer-world").unwrap();
    let mut primary = None;
    let mut preceding_origin = None;
    for phase in 0..2 {
        let settled = settle_compiler_transaction_cancellable(
            CompilerTransactionCancellation::new(),
            |_| {},
            || {
                let configured = crate::toolchain::CompilerDeploymentConfiguration::from_env()
                    .expect("the test uses its declared compiler deployment");
                let (endpoint, _, _) =
                    crate::toolchain::bind_admitted_extract_endpoint(&configured)
                        .expect("the matched compiler endpoint is admitted");
                let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint)
                    .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
                let world = crate::compiler_world::open_world(
                    crate::artifacts::ModuleCandidateOffer::compiler_world_inputs(
                        primary.clone(),
                        VisibleWorldBindings::from_published_bindings(0, std::iter::empty())?,
                        Arc::clone(&sources),
                        CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None)?,
                    )?,
                    &endpoint,
                )?;
                let plan = crate::compiler_world::plan_cell(&endpoint, &world,
                    Arc::new(tidepool_extract_cmd::WorldCellRecipe {
                        source: if phase == 0 { "import PublicationHelper ()" }
                            else { "import qualified PublicationHelper\nlet answer = PublicationHelper.second" }.into(),
                        template: include_str!("../../tests/fixtures/world-native/CellTemplate.hs").into(),
                        turn_templates: Vec::new(),
                    }), None,
                )?;
                assert_eq!(plan.items().len(), if phase == 0 { 1 } else { 2 });
                let origin =
                    CompilerTransactionOrigin::current().expect("actual compiler source operation");
                if let Some(previous) = &preceding_origin {
                    assert_ne!(
                        &origin, previous,
                        "B owns a separate actual physical source transaction"
                    );
                }
                let subject = WorldCellAdmissionSubject::for_source_transaction(
                    &origin,
                    [47; 16],
                    phase,
                    1,
                    1,
                    [b"actual source grant for crossed producer control".to_vec()],
                )?;
                let mut requests = vec![WorldCellReservationRequest::Prologue {
                    declaration: 90 + phase * 2,
                }];
                if phase == 1 {
                    requests.push(WorldCellReservationRequest::Bind {
                        value: 93,
                        bindings: vec![(
                            "answer".into(),
                            tidepool_repr::SessionVarId::from_extract(
                                tidepool_codegen::prepared_program::session_var_id(
                                    &tidepool_repr::SessionModule::val(tidepool_repr::Generation(
                                        93,
                                    ))
                                    .module_name(),
                                    "answer",
                                ),
                            ),
                        )],
                    });
                }
                let reservation = plan.reserve(plan.issue_admission_token(subject, requests)?)?;
                let checked = crate::compiler_world::admit_world_cell_program(
                    &endpoint,
                    reservation,
                    source.clone(),
                    source.clone(),
                    &origin,
                )?
                .map_err(|refusal| {
                    CompileError::ExtractFailed(format!(
                        "actual producer World refused: {refusal:?}"
                    ))
                })?;
                let emissions = checked.emitted_native_modules()?;
                let mut products = Vec::new();
                let mut physical_stages = Vec::new();
                let prepared =
                    crate::artifacts::prepare_world_cell_program_observing_home_contracts(
                        Arc::clone(&checked),
                        &mut |_, originals| {
                            let stage = originals.captured_stage();
                            physical_stages.push(Arc::clone(stage));
                            for entry in stage
                                .producer_certificates()
                                .iter()
                                .filter(|entry| &entry.descriptor.owner == owner)
                            {
                                if matches!(entry.payload, ArtifactPayload::Original(_)) {
                                    products.push((Arc::clone(entry), Arc::clone(stage)));
                                }
                            }
                        },
                    )?;
                Ok::<_, CompileError>((
                    prepared,
                    checked,
                    products,
                    physical_stages,
                    emissions,
                    origin,
                ))
            },
        );
        assert!(matches!(settled.close(), CompilerTransactionClose::Clean));
        let cancellation = settled
            .host_admission_cancellation()
            .expect("actual post-END owner")
            .clone();
        let (prepared, checked, products, physical_stages, emissions, origin) = settled
            .into_outcome()
            .action
            .expect("the real World source grant completes the selected producer phase");
        let program = prepared
            .seal(&cancellation)
            .expect("seal the same actual completed World output");
        let fresh_owner = ExactModuleIdentity {
            unit: "main".into(),
            module: tidepool_repr::SessionModule::lib(tidepool_repr::Generation(90 + phase * 2))
                .module_name(),
        };
        let declaration = program.items()[0]
            .checked_item()
            .planned_declaration()
            .expect("the actual prologue issues its fresh Original declaration");
        let stage = physical_stages
            .iter()
            .find(|stage| {
                stage.producer_certificates().iter().any(|entry| {
                    entry.descriptor.owner == fresh_owner
                        && matches!(entry.payload, ArtifactPayload::Original(_))
                })
            })
            .expect("the actual prologue's physical producer certificate is retained");
        assert_fresh_world_original_selection(
            stage,
            declaration.compiler_source_support(),
            &fresh_owner,
        );
        let closed_item_support = program.items()[0]
            .compiler_context()
            .compiler_source_support();
        assert_fresh_world_original_selection(stage, closed_item_support, &fresh_owner);
        let continuation = program
            .items()
            .last()
            .expect("the actual phase has a source item")
            .compiler_context();
        if phase == 0 {
            assert!(
                continuation.source_scope().admits(owner),
                "the first actual import issues SOURCE-H"
            );
            assert!(continuation.compiler_roles().get(owner).is_some());
            assert!(
                continuation
                    .compiler_roles()
                    .original_acquisition(owner)
                    .is_none(),
                "the empty import must issue TYPE-H without a native Original role"
            );
            assert!(
                !emissions
                    .iter()
                    .any(|stage| stage.owners().iter().any(|emitted| emitted == owner)),
                "the first phase must not already produce native H"
            );
            let attached = continuation
                .as_ref()
                .clone()
                .retain_issued_native_inputs(
                    &crate::declaration_context::OriginalCompilerInputs::from_issued_support(
                        private,
                    ),
                )
                .unwrap();
            assert!(attached
                .compiler_roles()
                .original_acquisition(owner)
                .is_none());
            assert!(
                attached
                    .compiler_source_support()
                    .native_offers()
                    .iter()
                    .any(|offer| offer.same_issuance(prior_offer)),
                "actual private A stays held before B"
            );
            assert!(
                attached
                    .compiler_source_support()
                    .native_availability()
                    .issued_row(prior_offer.original())
                    .is_some_and(|row| row.offers().any(|offer| offer.same_issuance(prior_offer))),
                "A must remain in actual private request availability, not only byte custody"
            );
            preceding_origin = Some(origin);
            primary = Some(Arc::new(attached));
            continue;
        }
        assert!(
            emissions
                .iter()
                .any(|stage| stage.owners().iter().any(|emitted| emitted == owner)),
            "the second actual phase must emit H's native module packet"
        );
        let ArtifactPayload::Original(prior_product) = &prior_offer.original_acquisition().payload
        else {
            unreachable!()
        };
        let mut actual = products
            .into_iter()
            .filter(|(entry, _)| !Arc::ptr_eq(entry, prior_offer.original_acquisition()));
        let (producer, stage) = actual
            .next()
            .expect("B must be a separate actual H completion, not cached A");
        assert!(
            actual.all(|(other, _)| Arc::ptr_eq(&other, &producer)),
            "one independent physical H product is carried through later stage projections"
        );
        let ArtifactPayload::Original(product) = &producer.payload else {
            unreachable!()
        };
        // Ordinary A and retained-Core B have separate version/replay domains.
        // B's actual output acquisition carries its completed own offer, while
        // its source identity remains in the authenticated canonical/Core proof.
        // Shared canonical/body facts do not equate their Original owners.
        // Equal-Original acquisition histories are covered by the independent
        // ordinary requests in authored_promotion_preserves_actual_offered_acquisition.
        assert_eq!(product.owner().unit, prior_product.owner().unit);
        assert_eq!(product.owner().module, prior_product.owner().module);
        assert_eq!(
            ArtifactEntry::canonical_artifact_id(product.module_interface()),
            ArtifactEntry::canonical_artifact_id(prior_product.module_interface()),
        );
        assert!(product.module_interface() == prior_product.module_interface());
        assert_eq!(
            Some(product.module_interface().source_sha256()),
            prior_product
                .source_admission()
                .map(|admission| admission.source_sha256()),
        );
        assert!(product.module_interface().core_bytes().is_some());
        assert!(product.source_admission().is_none());
        assert!(product.interface_bytes() == prior_product.interface_bytes());
        assert!(product.product_bytes() == prior_product.product_bytes());
        assert!(product.package_imports_bytes() == prior_product.package_imports_bytes());
        assert_ne!(
            product.owner().module_version,
            prior_product.owner().module_version,
            "retained-Core versions bind the authenticated canonical/body/dependency graph"
        );
        assert!(product.certification_bytes() != prior_product.certification_bytes());
        assert!(product.execution_source().is_none());
        assert!(prior_product.execution_source().is_some());
        assert!(product.source_admission().is_none());
        assert!(prior_product.source_admission().is_some());
        assert_current_producer_excludes_historical_original(
            stage.as_ref(),
            &producer,
            &prior_acquisition,
        );
        let offer = stage
            .source_selection()
            .completed_compiler_output_offer(owner)
            .unwrap()
            .expect("the actual physical B receipt retains its own contribution");
        assert_ne!(offer.original(), prior_offer.original());
        assert!(
            Arc::ptr_eq(offer.original_acquisition(), &producer),
            "B's completed receipt retains the actual prebinding producer acquisition"
        );
        assert!(offer.matches_direct_canonical_binding(
            producer.descriptor.id,
            ArtifactEntry::canonical_artifact_id(product.module_interface()),
            stage.issued_artifacts(),
        ));
        assert!(!producer.native_group_ordinals.is_empty());
        for ordinal in &producer.native_group_ordinals {
            assert!(
                stage
                    .source_selection()
                    .current_product(
                        NativeGroupKey {
                            artifact: producer.descriptor.id,
                            original_ordinal: *ordinal,
                        },
                        &producer,
                    )
                    .unwrap()
                    .is_some(),
                "every actual B group belongs to this current physical issuer"
            );
        }
        let physical = stage
            .issued_artifacts()
            .observed_payload(&offer.original())
            .expect("the actual physical cohort retains produced B separately from its prefix");
        assert!(
            Arc::ptr_eq(physical, offer.original_acquisition()),
            "the physical B scope and its own receipt carry the same acquired entry"
        );
        assert!(!offer.same_issuance(prior_offer));
        // The completed World owns its cold request separately from each
        // stage's narrowed native demand. Unused A stays with that actual
        // request owner; requiring its offer in B's stage would over-deliver
        // native permission that this source completion did not consume.
        let request = checked.primary_inputs().inputs();
        let retained_offer = request
            .compiler_source_support()
            .native_offers()
            .iter()
            .find(|offer| offer.same_issuance(prior_offer))
            .expect("the actual completed request retains A's original offer issuance");
        assert!(Arc::ptr_eq(
            retained_offer.original_acquisition(),
            &prior_acquisition,
        ));
        assert!(Arc::ptr_eq(
            request
                .artifacts()
                .observed_payload(&prior_offer.original())
                .unwrap(),
            &prior_acquisition,
        ));
        assert_eq!(
            retained_offer.proof().unwrap().selection.graph_selection(),
            &prior_graph,
            "the actual A offer retains its original scoped native closure",
        );
        let retained_graph = request.artifacts().capture_graph_selection();
        let retained_edges = request.artifacts().binding_dependencies();
        // Private group custody belongs to the opaque offer above. The
        // receiving view retains its exact carriers and their artifact edges.
        for carrier in prior_offer.artifact_carriers() {
            assert!(retained_graph.bindings.contains(carrier));
            let proof = prior_graph
                .selections
                .iter()
                .find(|proof| proof.selection == carrier.selection)
                .unwrap();
            assert!(retained_graph.selections.contains(proof));
            for edge in &proof.dependencies {
                if edge.0 == ArtifactBindingNode::Artifact(*carrier) {
                    assert!(retained_edges.contains(edge));
                }
            }
        }
        let private_availability = request.private_native_availability();
        assert!(private_availability.custody_contributions().iter().any(
            |(original, contribution)| {
                *original == prior_offer.original()
                    && contribution.offer().same_issuance(prior_offer)
                    && prior_graph
                        .native_groups
                        .iter()
                        .all(|group| contribution.contains_group(*group))
            }
        ));
        for artifacts in [
            stage.compiler_inputs().issued_artifacts(),
            continuation.compiler_source_support().artifacts(),
        ] {
            assert!(
                artifacts
                    .selected_native_groups()
                    .iter()
                    .all(|group| { group.artifact != prior_offer.original() }),
                "private A must not become an executable native selection"
            );
            assert!(artifacts
                .capture_graph_selection()
                .roots
                .iter()
                .all(|root| {
                    !matches!(root, ArtifactBindingNode::Group(group)
                    if group.binding.artifact() == prior_offer.original())
                }));
        }
        let receiving = primary.as_ref().unwrap();
        assert_whole_selected_row(
            receiving.compiler_source_support(),
            continuation.compiler_source_support(),
            owner,
        );
    }
}

fn assert_current_producer_excludes_historical_original(
    source: &crate::declaration_context::CapturedCompilerStage,
    current: &Arc<ArtifactEntry>,
    historical: &Arc<ArtifactEntry>,
) {
    assert_ne!(current.descriptor.id, historical.descriptor.id);
    assert_eq!(current.descriptor.owner, historical.descriptor.owner);
    let canonical = |entry: &ArtifactEntry| {
        let ArtifactPayload::Original(product) = &entry.payload else {
            unreachable!()
        };
        ArtifactEntry::canonical_artifact_id(product.module_interface())
    };
    assert_eq!(canonical(current), canonical(historical));
    let projection =
        CompilerInputProjection::from_issued_entries(&[Arc::clone(current)]).unwrap();
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
    let mut pending =
        crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            &projection,
            &source.issued_artifacts().metadata_snapshot(),
            &validation.inventory,
        )
        .unwrap();
    assert!(pending.has_unissued_current_native_products());
    let captured = crate::declaration_context::CapturedCompilerStage::issue(
        &mut pending,
        source.issued_artifacts(),
        vec![Arc::clone(current), Arc::clone(historical)],
        &mut validation,
    )
    .unwrap();
    for entry in [current, historical] {
        assert!(captured
            .issued_artifacts()
            .observed_payload(&entry.descriptor.id)
            .is_some_and(|retained| retained.same_issuing_authority(entry)));
    }
    assert_eq!(
        captured
            .producer_certificates()
            .iter()
            .filter(|entry| matches!(entry.payload, ArtifactPayload::Original(_)))
            .map(|entry| entry.descriptor.id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([current.descriptor.id]),
        "historical Original custody does not make it a current producer"
    );
    let receipt = captured
        .source_selection()
        .completed_compiler_output_offer(&current.descriptor.owner)
        .unwrap()
        .unwrap();
    assert!(receipt.original_acquisition().same_issuing_authority(current));
    assert_eq!(receipt.original(), current.descriptor.id);
}

fn assert_fresh_world_original_selection(
    stage: &crate::declaration_context::CapturedCompilerStage,
    selected: &CompilerSourceSupport,
    owner: &ExactModuleIdentity,
) {
    let producer = stage
        .producer_certificates()
        .iter()
        .find(|entry| {
            &entry.descriptor.owner == owner
                && matches!(entry.payload, ArtifactPayload::Original(_))
        })
        .expect("the new compiler owner has an actual prebinding Original certificate");
    let ArtifactPayload::Original(produced) = &producer.payload else {
        unreachable!()
    };
    let offer = stage
        .source_selection()
        .completed_compiler_output_offer(owner)
        .unwrap()
        .expect("the actual fresh physical receipt owns the new Original's contribution");
    let actual = selected
        .compiler_roles()
        .original_acquisition(owner)
        .expect("the public authored-declaration consumer selects its genuinely new Original row");
    let ArtifactPayload::Original(product) = &actual.payload else {
        unreachable!()
    };
    assert!(
        product.same_admission_and_transport(produced),
        "selected new Original keeps its independent actual producer acquisition"
    );
    assert!(Arc::ptr_eq(actual, offer.original_acquisition()));
    let row = &selected.selected[owner];
    let projected = selected
        .projection()
        .snapshot_roles()
        .get(owner)
        .expect("the closed output projection retains the newly selected owner");
    assert_eq!(projected.interface(), row.interface().0.artifact);
    assert_eq!(projected.original(), Some(producer.descriptor.id));
    assert_eq!(
        selected.recorded_rows().get(owner),
        Some(&row.recorded_row()),
        "the closed output's recorded index and acquired row share the same base"
    );
    assert!(
        offer.matches_direct_canonical_binding(
            producer.descriptor.id,
            row.interface().0.artifact,
            stage.issued_artifacts()
        ),
        "the selected canonical/Original pair belongs to the actual fresh producer"
    );
    assert!(offer
        .artifact_carriers()
        .contains(&row.original().unwrap().0));
    let mut canonical_producers = stage.producer_certificates().iter().filter(|entry| {
        entry.descriptor.id == row.interface().0.artifact
            && matches!(entry.payload, ArtifactPayload::Canonical(_))
    });
    let canonical = canonical_producers
        .next()
        .expect("the genuinely new owner has an actual canonical producer certificate");
    assert!(canonical_producers.next().is_none());
    assert!(Arc::ptr_eq(&row.interface().1, canonical));
    let expected_groups = offer
        .proof()
        .into_iter()
        .flat_map(|proof| proof.selection.snapshot().roots.keys())
        .filter_map(|root| match root {
            ArtifactBindingNode::Group(group)
                if group.binding.artifact() == producer.descriptor.id =>
            {
                Some((group.original_ordinal, *group))
            }
            _ => None,
        })
        .collect::<imbl::OrdMap<_, _>>();
    let InputNativeDecision::IssuedChoices(groups) =
        selected.compiler_roles().native_decision(owner)
    else {
        panic!("fresh capture issues its exact completed own-offer choices")
    };
    assert_eq!(groups, &expected_groups);
    for group in groups.values() {
        let support = selected
            .compiler_roles()
            .selected_native_support(owner, *group)
            .expect("the fresh selected group keeps its actual acquisition");
        assert_eq!(support.acquisitions().len(), 1);
        assert!(support.acquisitions()[0].same_issuance(offer));
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
