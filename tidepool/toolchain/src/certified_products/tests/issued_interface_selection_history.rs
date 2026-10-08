use super::*;
use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory, CompilerInputProjection};
use crate::declaration_context::{ExactDeclarationContext, OriginalCompilerInputs};

pub(super) fn issued_original(
    root: &Path,
    module: &str,
    interface: u8,
) -> crate::recovery_artifacts::CertifiedRecoveryProduct {
    std::fs::create_dir_all(root).unwrap();
    let source = format!("module {module} where");
    let input = root.join(format!("{module}.hs"));
    std::fs::write(&input, &source).unwrap();
    let bytes = tidepool_test_data::prepared_encode::encode_module_products(&[RawModuleProduct {
        unit: "main".into(),
        module: module.into(),
        interface: vec![interface],
        groups: vec![],
    }]);
    let packages = receipt_bytes(&value_array([
        value_text("TPPKGROOTS"),
        value_text("2"),
        value_array([
            value_text("main"),
            value_text(module),
            value_text(hex(&sha(&[interface]))),
        ]),
        value_array([]),
        value_array([]),
    ]));
    let bundles = receipt_bytes(&value_array([
        value_text("TPPKGBUNDLES"),
        Value::Integer(1.into()),
        value_array([value_array([
            value_text("main"),
            value_text(module),
            Value::Bytes(packages),
        ])]),
    ]));
    let parsed = ParsedModuleProducts::decode(&bytes, &bundles).unwrap();
    let mut complete = evidence(&source);
    complete.modules[0].module = module.into();
    let mut worker = complete.clone();
    worker.sources[0].path = input.clone();
    worker.modules[0].source = input.clone();
    let worker = serde_json::to_vec(&worker).unwrap();
    let mut accepted = receipt(&bytes, &complete, &source);
    accepted.module = module.into();
    accepted.skinny_iface_sha256 = sha(&[interface]);
    accepted.dependency_witness_sha256 = sha(&worker);
    let packet = CertifiedReceipt {
        source_recipe: WorkerExecutionSource::Ordinary,
        finalization: fixture_finalization_from_products(Some(root), &[accepted.clone()], &parsed),
        modules: vec![accepted],
        targets: BTreeMap::new(),
        packages: BTreeMap::new(),
    };
    let certified = certify_products(
        None,
        &packet,
        &parsed,
        &worker,
        &input,
        root,
        &CompletedSourceEvidence::from_normalized(complete, &source).unwrap(),
        &source,
        &[3; 32],
        &[root.to_path_buf()],
        None,
        None,
    )
    .unwrap();
    let original = certified.recovery_products.into_iter().next().unwrap();
    assert!(
        original
            .original_native()
            .unwrap()
            .matches_original(&original)
    );
    assert_eq!(
        original.module_interface().unwrap().interface_bytes(),
        &[interface]
    );
    original
}

fn available_inputs(
    root: &Path,
) -> (
    crate::artifact_inventory::ArtifactView,
    Vec<Arc<ArtifactEntry>>,
) {
    let fresh = issued_original(&root.join("fresh"), "Fresh", 0x42);
    let side = issued_original(&root.join("side"), "Side", 0x44);
    let other = issued_original(&root.join("other"), "Other", 0x45);
    let producer =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&[3; 32])
            .sha256();
    let fresh_native = Arc::new(
        ArtifactEntry::original_with_validation(
            producer,
            fresh.clone(),
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap(),
    );
    let fresh_canonical = Arc::new(ArtifactEntry::canonical(
        fresh.module_interface().unwrap().clone(),
    ));
    let side = Arc::new(ArtifactEntry::canonical(
        side.module_interface().unwrap().clone(),
    ));
    let other = Arc::new(ArtifactEntry::canonical(
        other.module_interface().unwrap().clone(),
    ));
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            vec![
                fresh_native.clone(),
                fresh_canonical,
                side.clone(),
                other.clone(),
            ],
            &BTreeSet::new(),
        )
        .unwrap();
    (view, vec![fresh_native, side, other])
}

#[test]
fn sparse_issued_interfaces_do_not_expand_to_unselected_custody() {
    let root = tempfile::tempdir().unwrap();
    let (view, entries) = available_inputs(root.path());
    for chosen in 1..=2 {
        let projection = CompilerInputProjection::from_issued_entries(&[
            entries[0].clone(),
            entries[chosen].clone(),
        ])
        .unwrap();
        let selection = CertifiedSourceSelection::from_compiler_projection(
            &projection,
            &view.metadata_snapshot(),
            &InventoryOperation::new(Default::default()),
        )
        .unwrap();
        let private = OriginalCompilerInputs::from_selection(&selection, &view).unwrap();
        let producer =
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&[3; 32])
                .sha256();
        let public = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products(producer, &[])
                .unwrap(),
        );
        let request = public
            .prepare_compilation(&root.path().join(format!("seed-{chosen}")), &[3; 32])
            .unwrap()
            .in_program_context_with_private_input(
                &root.path().join(format!("private-{chosen}")),
                public.clone(),
                &private,
            )
            .unwrap();
        assert!(request.context.interface_owners().is_empty());
        assert!(request.context.lexical_graph().is_empty());
        assert!(request.context.recovery_products().is_empty());
        assert!(request.groups.is_empty());
        let effective = request.compiler_inputs().unwrap();
        let expected = BTreeSet::from(["Fresh", if chosen == 1 { "Side" } else { "Other" }]);
        assert_eq!(
            effective
                .metadata
                .entries
                .keys()
                .map(|owner| owner.module.as_str())
                .collect::<BTreeSet<_>>(),
            expected,
            "unselected interface acquired a compiler role: chosen={chosen}"
        );
        assert_eq!(effective.metadata.entries.len(), 2);
        assert_eq!(effective.metadata.artifacts.len(), 4);
        let chosen_entry = &effective.metadata.entries[&entries[chosen].descriptor.owner];
        assert_eq!(chosen_entry.descriptor.id, entries[chosen].descriptor.id);
        assert_eq!(
            chosen_entry.descriptor.interface_sha256,
            sha(&[if chosen == 1 { 0x44 } else { 0x45 }])
        );
        assert!(matches!(
            chosen_entry.payload,
            crate::artifact_inventory::ArtifactPayload::Canonical(_)
        ));
        let selected_native = request.compiler_original_products().unwrap();
        assert_eq!(selected_native.len(), 1);
        let crate::artifact_inventory::ArtifactPayload::Original(original) = &entries[0].payload
        else {
            unreachable!()
        };
        assert_eq!(selected_native[0].owner(), original.owner());
    }
}

#[test]
fn omission_of_an_issued_interface_only_role_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (view, entries) = available_inputs(root.path());
    let projection =
        CompilerInputProjection::from_issued_entries(&[entries[0].clone(), entries[1].clone()])
            .unwrap();
    let selection = CertifiedSourceSelection::from_compiler_projection(
        &projection,
        &view.metadata_snapshot(),
        &InventoryOperation::new(Default::default()),
    )
    .unwrap();
    let without_side = view
        .select_roots(vec![entries[0].descriptor.id, entries[2].descriptor.id])
        .unwrap();
    assert!(
        !without_side
            .metadata_snapshot()
            .artifacts
            .contains_key(&entries[1].descriptor.id)
    );
    assert!(
        OriginalCompilerInputs::from_selection(&selection, &without_side).is_err(),
        "omitted issued Side interface must not be reconstructed from other custody"
    );
}

#[test]
fn compiler_issuer_refuses_missing_paired_canonical_artifact() {
    let root = tempfile::tempdir().unwrap();
    let (view, entries) = available_inputs(root.path());
    let projection =
        CompilerInputProjection::from_issued_entries(&[entries[0].clone(), entries[1].clone()])
            .unwrap();
    let mut metadata = view.metadata_snapshot();
    let crate::artifact_inventory::ArtifactPayload::Original(original) = &entries[0].payload else {
        unreachable!()
    };
    let paired = ArtifactEntry::canonical(original.module_interface().unwrap().clone())
        .descriptor
        .id;
    assert!(metadata.artifacts.remove(&paired).is_some());
    assert!(metadata.artifacts.contains_key(&entries[0].descriptor.id));
    assert!(matches!(
        CertifiedSourceSelection::from_compiler_projection(
            &projection,
            &metadata,
            &InventoryOperation::new(Default::default()),
        ),
        Err(CertificationError::Mismatch("compiler original projection"))
    ));
}
