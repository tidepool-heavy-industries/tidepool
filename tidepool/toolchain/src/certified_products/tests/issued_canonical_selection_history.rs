use super::*;
use crate::artifact_inventory::{ArtifactEntry, ArtifactInventory, CompilerInputProjection};
use crate::declaration_context::{ExactDeclarationContext, OriginalCompilerInputs};

fn issued_original(
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
    assert!(original
        .original_native()
        .unwrap()
        .matches_original(&original));
    assert_eq!(
        original.module_interface().unwrap().interface_bytes(),
        &[interface]
    );
    original
}

#[test]
fn issued_roles_preserve_chosen_canonical_among_distinct_archive_versions() {
    let root = tempfile::tempdir().unwrap();
    let originals = [
        issued_original(&root.path().join("old"), "Fresh", 0x42),
        issued_original(&root.path().join("new"), "Fresh", 0x43),
    ];
    let side = issued_original(&root.path().join("side"), "Side", 0x44);
    let producer =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&[3; 32])
            .sha256();
    let mut validation = PackageInterfaceValidation::default();
    let native = originals
        .iter()
        .map(|original| {
            Arc::new(
                ArtifactEntry::original_with_validation(
                    producer,
                    original.clone(),
                    &mut validation,
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let canonical = originals
        .iter()
        .map(|original| {
            Arc::new(ArtifactEntry::canonical(
                original.module_interface().unwrap().clone(),
            ))
        })
        .collect::<Vec<_>>();
    let side = Arc::new(ArtifactEntry::canonical(
        side.module_interface().unwrap().clone(),
    ));
    assert_ne!(canonical[0].descriptor.id, canonical[1].descriptor.id);
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            native
                .iter()
                .chain(&canonical)
                .cloned()
                .chain(std::iter::once(side.clone()))
                .collect(),
            &BTreeSet::new(),
        )
        .unwrap();
    let metadata = view.metadata_snapshot();
    assert_eq!(
        metadata
            .artifacts
            .values()
            .filter(|entry| matches!(
                entry.payload,
                crate::artifact_inventory::ArtifactPayload::Original(_)
            ))
            .count(),
        2
    );
    for chosen in 0..2 {
        let projection =
            CompilerInputProjection::from_issued_entries(&[native[chosen].clone(), side.clone()])
                .unwrap();
        let selection = CertifiedSourceSelection::from_compiler_projection(
            &projection,
            &metadata,
            &InventoryOperation::new(Default::default()),
        )
        .unwrap();
        let private =
            OriginalCompilerInputs::from_selection(&selection, &view).unwrap_or_else(|error| {
                panic!("exact canonical choice lost: chosen={chosen}, error={error:?}")
            });
        let public = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
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
        let selected = &effective.metadata.entries;
        let fresh = &selected[&crate::declaration_join::ExactModuleIdentity {
            unit: "main".into(),
            module: "Fresh".into(),
        }];
        assert_eq!(
            fresh.descriptor.interface_sha256,
            sha(&[0x42 + chosen as u8])
        );
        assert_eq!(fresh.descriptor.id, native[chosen].descriptor.id);
        let side_entry = &selected[&crate::declaration_join::ExactModuleIdentity {
            unit: "main".into(),
            module: "Side".into(),
        }];
        assert_eq!(side_entry.descriptor.id, side.descriptor.id);
        assert!(matches!(
            side_entry.payload,
            crate::artifact_inventory::ArtifactPayload::Canonical(_)
        ));
        let selected_native = request.compiler_original_products().unwrap();
        assert_eq!(selected_native.len(), 1);
        assert_eq!(selected_native[0].owner(), originals[chosen].owner());
        assert_eq!(
            effective
                .metadata
                .artifacts
                .values()
                .filter(|entry| matches!(
                    entry.payload,
                    crate::artifact_inventory::ArtifactPayload::Original(_)
                ))
                .count(),
            2
        );
    }
}
