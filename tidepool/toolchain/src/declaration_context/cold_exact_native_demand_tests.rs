//! Cold candidates are availability; exact demand retains its admitted pair.
use super::native_availability_tests::{
    fixture_support_with_issued_offers, native_context, product,
};
use super::*;
use crate::artifact_inventory::{
    IssuedNativeOffer, NativeArtifactDemand, NativeOriginalRequirement,
};
use crate::certified_products::{InputDecisionRefusal, NativeDemandPacket, ReceiptImportOwner};
use tidepool_repr::execution_schema::SymbolIdentity;

struct History {
    context: Arc<ExactDeclarationContext>,
    canonical: Arc<ArtifactEntry>,
    entries: Vec<Arc<ArtifactEntry>>,
    views: Vec<ArtifactView>,
    offers: Vec<IssuedNativeOffer>,
}

impl History {
    fn new() -> Self {
        let offer_operation = std::sync::Arc::new(
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
        );
        let mut offer_validation =
            crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(offer_operation);
        let products = [
            product("ColdExactHistory", 201),
            product("ColdExactHistory", 202),
        ];
        assert_eq!(
            products[0].module_interface(),
            products[1].module_interface()
        );
        let context = native_context(&products[0]);
        let producer = context.toolchain_identity_sha256();
        let canonical = Arc::new(ArtifactEntry::canonical(
            products[0].module_interface().clone(),
        ));
        let entries = products
            .into_iter()
            .map(|product| Arc::new(ArtifactEntry::original(producer, product).unwrap()))
            .collect::<Vec<_>>();
        assert_ne!(entries[0].descriptor.id, entries[1].descriptor.id);
        let inventory = ArtifactInventory::default();
        let base = inventory
            .admit_shared(&inventory.empty_view(), vec![canonical.clone()])
            .unwrap();
        let views = entries
            .iter()
            .map(|entry| inventory.admit_shared(&base, vec![entry.clone()]).unwrap())
            .collect::<Vec<_>>();
        let offers = entries
            .iter()
            .zip(&views)
            .map(|(entry, view)| {
                let proof =
                    NativeOriginalRequirement::from_selected_view(entry.descriptor.id, view)
                        .unwrap()
                        .unwrap();
                let issuer = issue_fixture_compiler_support(
                    CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap(),
                    view.clone(),
                )
                .unwrap();
                let offer = issuer
                    .issue_native_offer(&entry.descriptor.owner, Some(proof), &mut offer_validation)
                    .unwrap();
                assert!(offer.is_full());
                offer
            })
            .collect();
        Self {
            context,
            canonical,
            entries,
            views,
            offers,
        }
    }

    fn inputs(&self, selected: Option<usize>, reverse: bool) -> Arc<RequestCompilerInputs> {
        let (projection, view) = match selected {
            Some(index) => (
                CompilerInputProjection::from_issued_entries(&[self.entries[index].clone()])
                    .unwrap(),
                self.views[index]
                    .retain_custody(&self.views[1 - index])
                    .unwrap(),
            ),
            None => (
                CompilerInputProjection::from_issued_entries(&[self.canonical.clone()]).unwrap(),
                self.views[0].retain_custody(&self.views[1]).unwrap(),
            ),
        };
        let offers = if reverse {
            vec![self.offers[1].clone(), self.offers[0].clone()]
        } else {
            self.offers.clone()
        };
        let support = if selected.is_some() {
            fixture_support_with_issued_offers(projection, view, offers).unwrap()
        } else {
            // Each Full is checked in its own issuing view. Retention combines
            // those issued rows; it must not requalify competing new Fulls.
            let rows = self
                .views
                .iter()
                .zip(&self.offers)
                .map(|(view, offer)| {
                    let issuer = recover_fixture_compiler_support(
                        projection.clone(),
                        view.clone(),
                        [offer.clone()],
                    )
                    .unwrap();
                    RequestNativeAvailability::issue_checked(
                        issuer.compiler_roles(),
                        &issuer.compiler_metadata(),
                        issuer.artifacts(),
                        std::slice::from_ref(offer),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            let rows = rows[0].merge(&rows[1]).unwrap().into_rows();
            recover_fixture_compiler_support(projection, view, offers)
                .unwrap()
                .retain_normalized_availability(&rows)
                .unwrap()
        };
        inputs_with_support(&self.context, support)
    }

    fn reference(&self, index: usize, ordinal: u32) -> ReceiptImportOwner {
        let namespace = self.views[index]
            .capture_graph_selection()
            .namespace
            .into_iter()
            .map(|binding| (binding.artifact, binding))
            .collect::<BTreeMap<_, _>>();
        ReceiptImportOwner::OriginalBinding {
            unit: self.entries[index].descriptor.owner.unit.clone(),
            module: self.entries[index].descriptor.owner.module.clone(),
            interface: namespace[&self.canonical.descriptor.id],
            original: namespace[&self.entries[index].descriptor.id],
            binder: binder(&self.entries[index], ordinal),
        }
    }
}

fn inputs_with_support(
    context: &Arc<ExactDeclarationContext>,
    support: crate::artifact_inventory::CompilerSourceSupport,
) -> Arc<RequestCompilerInputs> {
    let context = context
        .as_ref()
        .clone()
        .rebuild(|facts| facts.compiler_support = support);
    Arc::new(
        ProtectedScaffoldRequirements::default()
            .compiler_inputs(&context, None)
            .unwrap(),
    )
}

fn binder(entry: &ArtifactEntry, ordinal: u32) -> SymbolIdentity {
    let ArtifactPayload::Original(product) = &entry.payload else {
        unreachable!()
    };
    crate::certified_products::original_available_groups(product)
        .unwrap()
        .find(|group| group.original_ordinal() == ordinal)
        .unwrap()
        .binders()[0]
        .clone()
}

// This is wire DATA, decoded by the production packet parser before issuance.
fn packet(
    reference: &ReceiptImportOwner,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> NativeDemandPacket {
    let ReceiptImportOwner::OriginalBinding {
        unit,
        module,
        interface,
        original,
        binder,
    } = reference
    else {
        unreachable!()
    };
    let pair = |binding: ArtifactBinding| {
        Value::Array(vec![
            Value::Bytes(binding.artifact.0.to_vec()),
            Value::Bytes(binding.selection.0.to_vec()),
        ])
    };
    let wire = Value::Array(vec![
        text("TPDEPENDENCYQUERY1"),
        Value::Integer(1u64.into()),
        Value::Array(vec![Value::Array(vec![
            text(unit),
            text(module),
            pair(*interface),
            pair(*original),
            Value::Array(vec![
                text(&binder.unit),
                text(&binder.module),
                text(&binder.namespace),
                text(&binder.occurrence),
                binder.record_parent.as_ref().map_or(Value::Null, text),
            ]),
        ])]),
    ]);
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&wire, &mut bytes).unwrap();
    NativeDemandPacket::decode(&bytes, operation).unwrap()
}

#[test]
fn cold_interface_only_candidates_refuse_nominal_and_original_role_escalation() {
    let history = History::new();
    for reverse in [false, true] {
        let cold = history.inputs(None, reverse);
        assert!(cold
            .projection()
            .roles()
            .iter()
            .all(|role| role.original().is_none()));
        for entry in &history.entries {
            let available = cold.native_availability(entry.descriptor.id).unwrap();
            assert_eq!(available.mode, NativeTransportMode::Full);
            assert!(available.permits(9001));
        }
        let roots = BTreeSet::from([history.canonical.descriptor.owner.clone()]);
        let mut validation = PackageInterfaceValidation::default();
        let projection = cold
            .demanded_compiler_projection(&roots, &validation.inventory)
            .unwrap();
        let error = cold
            .demanded_native_inputs(projection, &roots, &validation.inventory)
            .err()
            .expect("nominal direct demand cannot choose among historical bodies");
        assert!(
            matches!(&error, DemandNativeInputsError::Compiler(CompileError::ArtifactInventory(error)) if matches!(&error.failure, ArtifactInventoryFailure::NativeOwnerAmbiguity { owner } if owner == &history.canonical.descriptor.owner)),
            "actual refusal: {error:?}"
        );
        let request = packet(&history.reference(0, 9001), &validation.inventory);
        let error = cold
            .resolve_native_demand(&request, &[], &mut validation)
            .err()
            .expect("exact pair DATA cannot acquire an Original compiler role");
        assert!(
            matches!(&error, CompileError::CompilerEvidence(error) if matches!(error.as_ref(), crate::certified_products::CertificationError::InputDecision { reason: InputDecisionRefusal::RoleMismatch, .. })),
            "actual refusal: {error:?}"
        );
    }
}

#[test]
fn selected_original_packet_keeps_exact_old_group_and_refuses_foreign_acquisition() {
    let history = History::new();
    for reverse in [false, true] {
        let cold = history.inputs(Some(0), reverse);
        assert!(
            cold.artifacts()
                .artifact_ids()
                .contains(&history.entries[1].descriptor.id),
            "newer body remains custody, not the selected Original"
        );
        let mut validation = PackageInterfaceValidation::default();
        let reference = history.reference(0, 9001);
        let request = packet(&reference, &validation.inventory);
        let acquired = cold
            .resolve_native_demand(&request, &[], &mut validation)
            .unwrap();
        assert!(acquired.belongs_to(&cold));
        let expected = *history.offers[0]
            .issued_group_carriers()
            .unwrap()
            .keys()
            .find(|group| {
                group.binding.artifact() == history.entries[0].descriptor.id
                    && group.original_ordinal == 9001
            })
            .unwrap();
        assert_eq!(
            acquired
                .groups()
                .selected_rows()
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([expected])
        );
        assert_eq!(
            acquired.groups().selected_rows()[&expected]
                .product()
                .owner(),
            match &history.entries[0].payload {
                ArtifactPayload::Original(product) => product.owner(),
                _ => unreachable!(),
            }
        );
        let resolved = acquired.groups().resolved_original_bindings().unwrap();
        let roots = BTreeSet::from([history.canonical.descriptor.owner.clone()]);
        let foreign = PackageInterfaceValidation::default();
        let projection = cold
            .demanded_compiler_projection(&roots, &validation.inventory)
            .unwrap();
        let error = cold
            .demanded_native_inputs_for(
                projection,
                NativeRootDemand::Exact {
                    bindings: resolved,
                    operation: &validation.inventory,
                },
                &foreign.inventory,
            )
            .err()
            .expect("same pair cannot cross operation owners");
        assert!(matches!(error, DemandNativeInputsError::ForeignBasis));
        let other = history.inputs(Some(0), reverse);
        let projection = other
            .demanded_compiler_projection(&roots, &validation.inventory)
            .unwrap();
        let error = other
            .demanded_native_inputs_for(
                projection,
                NativeRootDemand::Exact {
                    bindings: resolved,
                    operation: &validation.inventory,
                },
                &validation.inventory,
            )
            .err()
            .expect("same facts cannot replace actual input basis");
        assert!(matches!(error, DemandNativeInputsError::ForeignBasis));
        let mut changed = reference.clone();
        let ReceiptImportOwner::OriginalBinding { original, .. } = &mut changed else {
            unreachable!()
        };
        let ReceiptImportOwner::OriginalBinding {
            original: other_binding,
            ..
        } = history.reference(1, 9001)
        else {
            unreachable!()
        };
        assert_ne!(
            original.selection, other_binding.selection,
            "mutation must change the actual scoped pair"
        );
        original.selection = other_binding.selection;
        let error = cold
            .resolve_native_demand(
                &packet(&changed, &validation.inventory),
                &[],
                &mut validation,
            )
            .err()
            .expect("equal-canonical other history cannot rewrite selected binding scope");
        assert!(
            matches!(&error, CompileError::CompilerEvidence(error) if matches!(error.as_ref(), crate::certified_products::CertificationError::InputDecision { reason: InputDecisionRefusal::ArtifactIdentityMismatch(_), .. })),
            "actual refusal: {error:?}"
        );
        assert!(
            cold.resolve_native_demand(&request, &[], &mut validation)
                .is_ok(),
            "refusals leave unchanged exact input usable"
        );
    }
}

#[test]
fn original_pair_membership_cannot_expand_explicit_empty_group_permission() {
    let product = product("ColdEmptyPermission", 203);
    let context = native_context(&product);
    let entry =
        Arc::new(ArtifactEntry::original(context.toolchain_identity_sha256(), product).unwrap());
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![entry.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let support = issue_fixture_compiler_support(projection, view).unwrap();
    let cold = inputs_with_support(&context, support);
    let row = cold.native_availability(entry.descriptor.id).unwrap();
    assert!(row.issued_ordinals().is_empty());
    let primary = cold
        .compiler_source_support()
        .compiler_primary_input(&entry.descriptor.owner)
        .unwrap()
        .unwrap();
    let reference = ReceiptImportOwner::OriginalBinding {
        unit: entry.descriptor.owner.unit.clone(),
        module: entry.descriptor.owner.module.clone(),
        interface: primary.interface(),
        original: primary.original().unwrap(),
        binder: binder(&entry, 9001),
    };
    let mut validation = PackageInterfaceValidation::default();
    let membership = crate::certified_products::AuthenticatedOriginalBindings::issue(
        cold.shared_compiler_source_support(),
        validation.inventory.clone(),
        [&reference],
    )
    .unwrap();
    assert!(
        membership.get(&reference).is_some(),
        "the pair and binder really are authenticated members"
    );
    let error = cold
        .resolve_native_demand(
            &packet(&reference, &validation.inventory),
            &[],
            &mut validation,
        )
        .err()
        .expect("metadata membership grants no missing body permission");
    assert!(
        matches!(&error, CompileError::CompilerEvidence(error) if matches!(error.as_ref(), crate::certified_products::CertificationError::OriginalAvailability(refusal) if refusal.original_ordinal == 9001 && &refusal.owner == row.original_owner())),
        "actual refusal: {error:?}"
    );
}

#[test]
fn exact_parent_demand_keeps_historical_child_under_interface_only_namespace() {
    let offer_operation = std::sync::Arc::new(
        tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
    );
    let mut offer_validation =
        crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(offer_operation);
    let history = History::new();
    let ArtifactPayload::Original(old) = &history.entries[0].payload else {
        unreachable!()
    };
    let producer = history.context.toolchain_identity_sha256();
    let parent = crate::certified_products::fixture_finalized_product(
        crate::certified_products::tests::original_groups_fixture_with_interface(
            "ColdExactParent",
            vec![(
                7,
                vec![crate::certified_products::PendingImportOwner::Source {
                    owner: old.owner().clone(),
                    original_ordinal: 9001,
                    binder: binder(&history.entries[0], 9001),
                }],
            )],
            204,
            &BTreeMap::new(),
            b"parent canonical interface".to_vec(),
        ),
        producer,
    );
    let parent =
        crate::certified_products::tests::recovered_witness_fixtures(&[old.clone(), parent])
            .remove(1)
            .product;
    let parent = Arc::new(ArtifactEntry::original(producer, parent).unwrap());
    let inventory = history.views[0].inventory();
    let parent_view = inventory
        .admit_shared(&history.views[0], vec![parent.clone()])
        .unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(parent.descriptor.id, &parent_view)
        .unwrap()
        .unwrap();
    let issuer = issue_fixture_compiler_support(
        CompilerInputProjection::from_issued_entries(&[history.entries[0].clone(), parent.clone()])
            .unwrap(),
        parent_view.clone(),
    )
    .unwrap();
    let parent_offer = issuer
        .issue_native_offer(&parent.descriptor.owner, Some(proof), &mut offer_validation)
        .unwrap();
    assert!(parent_offer.is_full());
    let expected = *parent_offer
        .issued_group_carriers()
        .unwrap()
        .keys()
        .find(|group| {
            group.binding.artifact() == history.entries[0].descriptor.id
                && group.original_ordinal == 9001
        })
        .unwrap();
    let expected_parent = *parent_offer
        .issued_group_carriers()
        .unwrap()
        .keys()
        .find(|group| {
            group.binding.artifact() == parent.descriptor.id && group.original_ordinal == 7
        })
        .unwrap();
    let projection =
        CompilerInputProjection::from_issued_entries(&[parent.clone(), history.canonical.clone()])
            .unwrap();
    let custody = parent_view.retain_custody(&history.views[1]).unwrap();
    let old_offers = [parent_offer.clone(), history.offers[0].clone()];
    let old_support = recover_fixture_compiler_support(
        projection.clone(),
        parent_view.clone(),
        old_offers.iter().cloned(),
    )
    .unwrap();
    let old_rows = RequestNativeAvailability::issue_checked(
        old_support.compiler_roles(),
        &old_support.compiler_metadata(),
        old_support.artifacts(),
        &old_offers,
    )
    .unwrap();
    let child_projection =
        CompilerInputProjection::from_issued_entries(&[history.canonical.clone()]).unwrap();
    let new_support = recover_fixture_compiler_support(
        child_projection,
        history.views[1].clone(),
        [history.offers[1].clone()],
    )
    .unwrap();
    let new_rows = RequestNativeAvailability::issue_checked(
        new_support.compiler_roles(),
        &new_support.compiler_metadata(),
        new_support.artifacts(),
        std::slice::from_ref(&history.offers[1]),
    )
    .unwrap();
    let rows = old_rows.merge(&new_rows).unwrap().into_rows();
    let support = recover_fixture_compiler_support(
        projection,
        custody,
        [
            parent_offer,
            history.offers[0].clone(),
            history.offers[1].clone(),
        ],
    )
    .unwrap()
    .retain_normalized_availability(&rows)
    .unwrap();
    let cold = inputs_with_support(&history.context, support);
    let child_role = &cold.projection().snapshot_roles()[&history.canonical.descriptor.owner];
    assert!(child_role.original().is_none());
    for entry in &history.entries {
        assert!(
            cold.native_availability(entry.descriptor.id)
                .unwrap()
                .permits(9001),
            "both child histories remain available before exact parent demand"
        );
    }
    let primary = cold
        .compiler_source_support()
        .compiler_primary_input(&parent.descriptor.owner)
        .unwrap()
        .unwrap();
    let reference = ReceiptImportOwner::OriginalBinding {
        unit: parent.descriptor.owner.unit.clone(),
        module: parent.descriptor.owner.module.clone(),
        interface: primary.interface(),
        original: primary.original().unwrap(),
        binder: binder(&parent, 7),
    };
    let mut validation = PackageInterfaceValidation::default();
    let acquired = cold
        .resolve_native_demand(
            &packet(&reference, &validation.inventory),
            &[],
            &mut validation,
        )
        .unwrap();
    let rows = acquired.groups().selected_rows();
    assert_eq!(
        rows.keys().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([expected_parent, expected])
    );
    assert_eq!(rows[&expected_parent].dependencies(), &[expected]);
    assert!(rows.contains_key(&expected));
    assert_eq!(rows[&expected].product().owner(), old.owner());
    assert!(!rows
        .keys()
        .any(|binding| binding.binding.artifact() == history.entries[1].descriptor.id));

    // The parent's authenticated edge needs the old payload, even when the
    // other exact version and the common canonical bytes are both present.
    let missing = inventory
        .admit_shared(&history.views[1], vec![parent.clone()])
        .err()
        .expect("newer same-canonical custody cannot fill the exact old child edge");
    assert!(
        matches!(&missing, CompileError::ArtifactInventory(error) if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency { artifact, required, .. } if *artifact == parent.descriptor.id && required == &history.entries[0].descriptor.owner)),
        "actual refusal: {missing:?}"
    );
    assert!(cold
        .resolve_native_demand(
            &packet(&reference, &validation.inventory),
            &[],
            &mut validation
        )
        .is_ok());
}
