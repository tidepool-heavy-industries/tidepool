use super::*;

use crate::artifact_inventory::CanonicalProducerIdentity;
use crate::declaration_context::{
    ExactCompilationRequest, ExactDeclarationContext, ExactProductAdmission, ExactSourceAdmission,
};
use crate::declaration_join::NativeAuthoredDeclarationAdmission;
use crate::recovery_artifacts::CertifiedRecoveryProduct;
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence};
use tidepool_repr::{Generation, SessionModule};

fn planned_history_property_config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 16;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 1024;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

pub(super) struct IssuedOriginal {
    pub(super) request: ExactCompilationRequest,
    pub(super) source: ExactSourceAdmission,
    pub(super) certified: CertifiedProducts,
}

fn planned_interface(module: &str) -> Vec<u8> {
    format!("planned-interface:main:{module}").into_bytes()
}

fn assert_canonical_materialization_premises(context: &ExactDeclarationContext) {
    let products = context.recovery_products();
    let mut interfaces = BTreeMap::new();
    for product in &products {
        let interface = product.module_interface().unwrap();
        let key = interface.interface_sha256();
        let value = (
            (&product.owner().unit, &product.owner().module),
            interface.package_imports_bytes(),
        );
        if let Some(previous) = interfaces.insert(key, value) {
            assert_eq!(
                previous, value,
                "fixture modules sharing interface bytes must have the same canonical owner and package sidecar"
            );
        }
    }
}

fn planned_product_sidecar(module: &str, body_tag: u8) -> Vec<u8> {
    let groups = [7, 11]
        .into_iter()
        .map(|ordinal| {
            let mut wire = testing::wire_program();
            let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0]
            else {
                unreachable!()
            };
            top.identity = SymbolIdentity {
                unit: "main".into(),
                module: module.into(),
                namespace: "value".into(),
                occurrence: format!("entry_{ordinal}"),
                record_parent: None,
            };
            top.binding.rhs =
                tidepool_repr::execution_schema::HeapRhs::Bytes(vec![ordinal as u8, body_tag]);
            let top = top.clone();
            wire.bindings = vec![tidepool_repr::execution_schema::Group::Recursive(vec![top])];
            wire.expressions.nodes.clear();
            wire.globals.clear();
            testing::projected_group(wire, ordinal).unwrap()
        })
        .collect();
    tidepool_test_data::prepared_encode::encode_module_products(&[RawModuleProduct {
        unit: "main".into(),
        module: module.into(),
        interface: planned_interface(module),
        groups,
    }])
}

pub(super) fn planned_product_sidecar_with_historical_child(
    module: &str,
    defined_occurrence: &str,
    child: &CachedHomeOwner,
    child_occurrence: &str,
    body_tag: u8,
) -> Vec<u8> {
    let groups = [7, 11]
        .into_iter()
        .map(|ordinal| {
            let mut wire = testing::wire_program();
            let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0]
            else {
                unreachable!()
            };
            let occurrence = if ordinal == 7 {
                defined_occurrence
            } else {
                "entry_11"
            };
            top.identity = SymbolIdentity {
                unit: "main".into(),
                module: module.into(),
                namespace: "value".into(),
                occurrence: occurrence.into(),
                record_parent: None,
            };
            top.binding.rhs =
                tidepool_repr::execution_schema::HeapRhs::Bytes(vec![ordinal as u8, body_tag]);
            let top = top.clone();
            wire.bindings = vec![tidepool_repr::execution_schema::Group::Recursive(vec![top])];
            wire.expressions.nodes.clear();
            wire.globals = if ordinal == 7 {
                vec![tidepool_repr::execution_schema::GlobalDecl {
                    identity: SymbolIdentity {
                        unit: child.unit.clone(),
                        module: child.module.clone(),
                        namespace: "value".into(),
                        occurrence: child_occurrence.into(),
                        record_parent: None,
                    },
                    rep: tidepool_repr::execution_schema::RuntimeRep::LiftedRef,
                    entry_signature: None,
                    required_evaluated: false,
                    required_generation: None,
                }]
            } else {
                vec![]
            };
            testing::projected_group(wire, ordinal).unwrap()
        })
        .collect();
    tidepool_test_data::prepared_encode::encode_module_products(&[RawModuleProduct {
        unit: "main".into(),
        module: module.into(),
        interface: planned_interface(module),
        groups,
    }])
}

pub(super) fn issue_planned_original_with_historical_child(
    root: &Path,
    context: &ExactDeclarationContext,
    module: SessionModule,
    producer: &[u8],
    include: &[PathBuf],
    defined_occurrence: &str,
    child: &CachedHomeOwner,
    child_occurrence: &str,
    child_source_path: &Path,
    child_source: &str,
) -> Result<IssuedOriginal, crate::CompileError> {
    let module_name = module.module_name();
    let source = format!(
        "module {module_name} where\nimport {} ({child_occurrence})\n{defined_occurrence} = {child_occurrence}\nentry_11 = 4\n",
        child.module,
    );
    let source_root = root.join(format!("planned-with-child-{module_name}"));
    std::fs::create_dir_all(&source_root).unwrap();
    let input = source_root.join("module.hs");
    std::fs::write(&input, &source).unwrap();
    let mut admitted = evidence(&source);
    admitted.modules[0].module = module_name.clone();
    admitted.modules[0].imports = vec![crate::cache::ModuleImportEvidence {
        qualifier: crate::cache::ImportQualifier::Unqualified,
        module: child.module.clone(),
        boot: false,
        selected: Some(child_source_path.to_path_buf()),
    }];
    admitted.sources.push(SourceEvidence {
        path: child_source_path.to_path_buf(),
        sha256: hex(&sha(child_source.as_bytes())),
    });
    admitted.resolutions.push(crate::cache::ResolutionEvidence {
        qualifier: crate::cache::ImportQualifier::Unqualified,
        module: child.module.clone(),
        boot: false,
        selected: Some(child_source_path.to_path_buf()),
        candidates: vec![child_source_path.to_path_buf()],
    });
    let normalized = CompletedSourceEvidence::from_normalized(admitted.clone(), &source).unwrap();
    assert_canonical_materialization_premises(context);
    let context = Arc::new(context.clone());
    let request = context
        .prepare_compilation(
            &root.join(format!("planned-inputs-{module_name}")),
            producer,
        )
        .unwrap();
    let receipt_root = source_root.join(".exact-compilations").join("source");
    std::fs::create_dir_all(&receipt_root).unwrap();
    let snapshot = receipt_root.join("source.hs");
    std::fs::write(&snapshot, &source).unwrap();
    let mut worker = admitted.clone();
    worker.sources[0].path = input.clone();
    let exact_receipt = value_array([
        value_text("TPEXACTCOMPILE"),
        value_text("3"),
        value_text(&request.request_sha256),
        value_text(hex(&request.semantic_sha256)),
        value_text(input.to_string_lossy()),
        value_text(hex(&sha(source.as_bytes()))),
        value_text(snapshot.to_string_lossy()),
        value_text(String::from_utf8(serde_json::to_vec(&worker).unwrap()).unwrap()),
        value_array([value_array([
            value_text("main"),
            value_text(&module_name),
            Value::Bool(false),
            value_array([value_array([
                value_text("none"),
                value_text(&child.module),
                Value::Bool(false),
                value_text(&child.unit),
            ])]),
        ])]),
        value_array([value_array([]), Value::Null]),
    ]);
    std::fs::write(
        receipt_root.join("receipt.cbor"),
        receipt_bytes(&exact_receipt),
    )
    .unwrap();
    worker.cache_safe = false;
    worker.selection_complete = false;
    let evidence_bytes = serde_json::to_vec(&worker).unwrap();
    let source_admission = request
        .admit_source(&input, &source, &evidence_bytes)
        .unwrap();
    let authored =
        NativeAuthoredDeclarationAdmission::from_planned(&module, &source_admission).unwrap();
    let exact = ExactProductAdmission {
        request: &request,
        source: &source_admission,
    };
    let bytes = planned_product_sidecar_with_historical_child(
        &module_name,
        defined_occurrence,
        child,
        child_occurrence,
        9,
    );
    let package_bytes =
        empty_package_bundle_for_interface(&module_name, &planned_interface(&module_name));
    let parsed = ParsedModuleProducts::decode(&bytes, &package_bytes).unwrap();
    let binder = SymbolIdentity {
        unit: child.unit.clone(),
        module: child.module.clone(),
        namespace: "value".into(),
        occurrence: child_occurrence.into(),
        record_parent: None,
    };
    let mut accepted = receipt(&bytes, &admitted, &source);
    accepted.module = module_name.clone();
    accepted.skinny_iface_sha256 = sha(&planned_interface(&module_name));
    accepted.groups = vec![
        AcceptedGroup {
            original_ordinal: 7,
            globals: vec![AcceptedGlobal {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                owner: ReceiptImportOwner::Source {
                    unit: child.unit.clone(),
                    module: child.module.clone(),
                    module_version: Some(child.module_version.clone()),
                    original_ordinal: 7,
                    binder,
                },
            }],
        },
        AcceptedGroup {
            original_ordinal: 11,
            globals: vec![],
        },
    ];
    accepted.interface_requirements.insert(
        (child.unit.clone(), child.module.clone()),
        child.skinny_iface_sha256,
    );
    accepted.dependency_witness_sha256 = sha(&evidence_bytes);
    let packet = CertifiedReceipt {
        source_recipe: WorkerExecutionSource::ExactUnavailable(
            SourceRecipeUnavailable::IncompleteSourceEvidence,
        ),
        finalization: fixture_finalization_from_products(
            Some(root),
            std::slice::from_ref(&accepted),
            &parsed,
        ),
        modules: vec![accepted],
        targets: BTreeMap::new(),
        packages: BTreeMap::new(),
    };
    let certified = certify_products(
        None,
        &packet,
        &parsed,
        &evidence_bytes,
        &input,
        root,
        &normalized,
        &source,
        producer,
        include,
        Some(&exact),
        Some(&authored),
    )
    .map_err(|error| crate::CompileError::CompilerEvidence(Box::new(error)))?;
    Ok(IssuedOriginal {
        request,
        source: source_admission,
        certified,
    })
}

fn issue_planned_original(
    root: &Path,
    module: SessionModule,
    producer: &[u8],
    include: &[PathBuf],
    source_tag: u8,
    body_tag: u8,
) -> IssuedOriginal {
    issue_planned_original_with_source_value(
        root, module, producer, include, source_tag, body_tag, 3,
    )
}

pub(super) fn issue_planned_original_with_source_value(
    root: &Path,
    module: SessionModule,
    producer: &[u8],
    include: &[PathBuf],
    source_tag: u8,
    body_tag: u8,
    source_value: u8,
) -> IssuedOriginal {
    let module_name = module.module_name();
    let source = format!("module {module_name} where\nentry_7 = {source_value}\nentry_11 = 4\n");
    let source_root = root.join(format!("source-{source_tag}"));
    std::fs::create_dir(&source_root).unwrap();
    let input = source_root.join("module.hs");
    std::fs::write(&input, &source).unwrap();
    let mut admitted = evidence(&source);
    admitted.modules[0].module = module_name.clone();
    let normalized = CompletedSourceEvidence::from_normalized(admitted.clone(), &source).unwrap();
    let canonical_producer = CanonicalProducerIdentity::from_producer_bytes(producer).sha256();
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products(canonical_producer, &[])
            .unwrap(),
    );
    let request = context
        .prepare_compilation(&root.join(format!("inputs-{source_tag}")), producer)
        .unwrap();
    let receipt_root = source_root.join(".exact-compilations").join("source");
    std::fs::create_dir_all(&receipt_root).unwrap();
    let snapshot = receipt_root.join("source.hs");
    std::fs::write(&snapshot, &source).unwrap();
    let mut worker = admitted.clone();
    worker.sources[0].path = input.clone();
    let exact_receipt = value_array([
        value_text("TPEXACTCOMPILE"),
        value_text("3"),
        value_text(&request.request_sha256),
        value_text(hex(&request.semantic_sha256)),
        value_text(input.to_string_lossy()),
        value_text(hex(&sha(source.as_bytes()))),
        value_text(snapshot.to_string_lossy()),
        value_text(String::from_utf8(serde_json::to_vec(&worker).unwrap()).unwrap()),
        value_array([value_array([
            value_text("main"),
            value_text(&module_name),
            Value::Bool(false),
            value_array([]),
        ])]),
        value_array([value_array([]), Value::Null]),
    ]);
    std::fs::write(
        receipt_root.join("receipt.cbor"),
        receipt_bytes(&exact_receipt),
    )
    .unwrap();
    worker.cache_safe = false;
    worker.selection_complete = false;
    let evidence_bytes = serde_json::to_vec(&worker).unwrap();
    let source_admission = request
        .admit_source(&input, &source, &evidence_bytes)
        .unwrap();
    let authored =
        NativeAuthoredDeclarationAdmission::from_planned(&module, &source_admission).unwrap();
    let exact = ExactProductAdmission {
        request: &request,
        source: &source_admission,
    };
    let bytes = planned_product_sidecar(&module_name, body_tag);
    let package_bytes =
        empty_package_bundle_for_interface(&module_name, &planned_interface(&module_name));
    let parsed = ParsedModuleProducts::decode(&bytes, &package_bytes).unwrap();
    let mut accepted = receipt(&bytes, &admitted, &source);
    accepted.module = module_name.clone();
    accepted.skinny_iface_sha256 = sha(&planned_interface(&module_name));
    accepted.groups = [7, 11]
        .into_iter()
        .map(|original_ordinal| AcceptedGroup {
            original_ordinal,
            globals: vec![],
        })
        .collect();
    accepted.dependency_witness_sha256 = sha(&evidence_bytes);
    let packet = CertifiedReceipt {
        source_recipe: WorkerExecutionSource::ExactUnavailable(
            SourceRecipeUnavailable::IncompleteSourceEvidence,
        ),
        finalization: fixture_finalization_from_products(
            Some(root),
            std::slice::from_ref(&accepted),
            &parsed,
        ),
        modules: vec![accepted],
        targets: BTreeMap::new(),
        packages: BTreeMap::new(),
    };
    let certified = certify_products(
        None,
        &packet,
        &parsed,
        &evidence_bytes,
        &input,
        root,
        &normalized,
        &source,
        producer,
        include,
        Some(&exact),
        Some(&authored),
    )
    .unwrap();
    assert_eq!(
        certified.recovery_products[0]
            .module_interface()
            .unwrap()
            .origin(),
        CanonicalOrigin::NativeAuthoredDeclaration {
            generation: module.gen.0
        }
    );
    IssuedOriginal {
        request,
        source: source_admission,
        certified,
    }
}

#[test]
fn selected_planned_original_survives_multiversion_custody_at_consumer_boundary() {
    let root = tempfile::tempdir().unwrap();
    let module = SessionModule::lib(Generation(7));
    let module_name = module.module_name();
    let producer = [0x35; 32];
    let include = [root.path().to_path_buf()];

    // A small generated history retains three compiler-issued originals for
    // one canonical module while the last exact selection names one version.
    let history = [
        issue_planned_original(root.path(), module, &producer, &include, 1, 1),
        issue_planned_original(root.path(), module, &producer, &include, 2, 2),
        issue_planned_original(root.path(), module, &producer, &include, 3, 3),
    ];
    let current = history.last().unwrap();
    let custody = history
        .iter()
        .flat_map(|issued| issued.certified.recovery_products.iter().cloned())
        .collect::<Vec<CertifiedRecoveryProduct>>();
    let all_owners = custody
        .iter()
        .map(|product| product.owner().clone())
        .collect::<BTreeSet<_>>();
    let selected_owner = current.certified.recovery_products[0].owner().clone();
    assert_eq!(all_owners.len(), 3);
    assert!(all_owners
        .iter()
        .all(|owner| { owner.unit == "main" && owner.module == module_name }));
    let canonical_interface = history[0].certified.recovery_products[0]
        .module_interface()
        .unwrap();
    assert!(history.iter().all(|issued| {
        issued.certified.recovery_products[0]
            .module_interface()
            .unwrap()
            == canonical_interface
    }));

    // Source selection is supplied by the final compiler certification;
    // inventory is independently assembled from all retained originals.
    for ordered in [custody.clone(), custody.iter().rev().cloned().collect()] {
        let view = crate::declaration_context::certified_product_artifact_view(
            CanonicalProducerIdentity::from_producer_bytes(&producer).sha256(),
            &ordered,
            &[],
            None,
        )
        .unwrap();
        let originals = current
            .certified
            .source_selection
            .selected_original_closure(&view)
            .unwrap();
        assert_eq!(originals.products().len(), 1);
        assert_eq!(originals.products()[0].owner(), &selected_owner);

        let exports = ["entry_7", "entry_11"].map(|occurrence| {
            serde_json::json!({
                "kind": "value",
                "head": {
                    "unit": "main",
                    "module": module_name,
                    "namespace": "value",
                    "occurrence": occurrence,
                    "record_parent": null
                },
                "children": []
            })
        });
        let inventory = serde_json::json!({
            "original_unit": "main",
            "original_module": module_name.clone(),
            "interface_fingerprint": "1234567890abcdef1234567890abcdef",
            "selection": {
                "exports": exports,
                "instances": { "classes": [], "families": [] },
                "family_closure": []
            }
        });
        let exact = ExactProductAdmission {
            request: &current.request,
            source: &current.source,
        };
        let certified = crate::declaration_join::certify_same_offer_planned_declaration(
            module,
            &format!("module {module_name} where\nentry_7 = 3\nentry_11 = 4\n"),
            &producer,
            &originals,
            &view,
            &exact,
            &include,
            None,
            &serde_json::to_vec(&inventory).unwrap(),
        )
        .unwrap();
        assert_eq!(certified.product().owner(), originals.products()[0].owner());

        let wrong_source = format!("module {module_name} where\nentry_7 = 2\nentry_11 = 3\n");
        assert!(
            crate::declaration_join::certify_same_offer_planned_declaration(
                module,
                &wrong_source,
                &producer,
                &originals,
                &view,
                &exact,
                &include,
                None,
                &serde_json::to_vec(&inventory).unwrap(),
            )
            .is_err()
        );
    }
}

proptest! {
    #![proptest_config(planned_history_property_config())]
    #[test]
    fn selected_planned_original_matches_generated_issue_and_retention_history(
        (older, current) in (1u8..8, 1u8..8).prop_filter(
            "history must retain distinct older and current versions",
            |(older, current)| older != current,
        ),
        repeated in prop::collection::vec(1u8..8, 0..3),
        reverse_custody in any::<bool>(),
    ) {
        let root = tempfile::tempdir().unwrap();
        let module = SessionModule::lib(Generation(7));
        let module_name = module.module_name();
        let producer = [0x36; 32];
        let include = [root.path().to_path_buf()];
        let tags = std::iter::once(older)
            .chain(repeated)
            .chain(std::iter::once(current))
            .collect::<Vec<_>>();

        // The flat model is keyed by the authored recipe's body tag; it does
        // not use inventory enumeration or infer selection from the view.
        let mut model_versions = BTreeSet::new();
        let mut issued_by_tag = BTreeMap::new();
        for (source_tag, tag) in tags.iter().enumerate() {
            model_versions.insert(*tag);
            let issued = issue_planned_original(
                root.path(), module, &producer, &include, source_tag as u8, *tag,
            );
            issued_by_tag.insert(*tag, issued);
        }
        let selected_issue = issued_by_tag.get(&current).unwrap();
        let selected_owner = selected_issue.certified.recovery_products[0]
            .owner()
            .clone();
        let mut custody = issued_by_tag
            .values()
            .map(|issued| issued.certified.recovery_products[0].clone())
            .collect::<Vec<_>>();
        if reverse_custody {
            custody.reverse();
        }
        assert_eq!(custody.len(), model_versions.len());
        assert_eq!(
            custody
                .iter()
                .map(|product| product.owner().clone())
                .collect::<BTreeSet<_>>()
                .len(),
            model_versions.len(),
            "distinct authored bodies issue distinct native owners"
        );
        let view = crate::declaration_context::certified_product_artifact_view(
            CanonicalProducerIdentity::from_producer_bytes(&producer).sha256(),
            &custody,
            &[],
            None,
        )
        .unwrap();
        let selected = selected_issue
            .certified
            .source_selection
            .selected_original_closure(&view)
            .unwrap();
        prop_assert_eq!(selected.products().len(), 1);
        prop_assert_eq!(selected.products()[0].owner(), &selected_owner);
        prop_assert_eq!(selected_owner.module.as_str(), module_name.as_str());
        let exports = ["entry_7", "entry_11"].map(|occurrence| {
            serde_json::json!({
                "kind": "value",
                "head": {
                    "unit": "main",
                    "module": module_name,
                    "namespace": "value",
                    "occurrence": occurrence,
                    "record_parent": null
                },
                "children": []
            })
        });
        let inventory = serde_json::json!({
            "original_unit": "main",
            "original_module": module_name,
            "interface_fingerprint": "1234567890abcdef1234567890abcdef",
            "selection": {
                "exports": exports,
                "instances": { "classes": [], "families": [] },
                "family_closure": []
            }
        });
        let exact = ExactProductAdmission {
            request: &selected_issue.request,
            source: &selected_issue.source,
        };
        let source = format!("module {module_name} where\nentry_7 = 3\nentry_11 = 4\n");
        let joined = crate::declaration_join::certify_same_offer_planned_declaration(
            module,
            &source,
            &producer,
            &selected,
            &view,
            &exact,
            &include,
            None,
            &serde_json::to_vec(&inventory).unwrap(),
        );
        prop_assert!(joined.is_ok());
        let drifted = format!("module {module_name} where\nentry_7 = 5\nentry_11 = 4\n");
        prop_assert!(crate::declaration_join::certify_same_offer_planned_declaration(
            module,
            &drifted,
            &producer,
            &selected,
            &view,
            &exact,
            &include,
            None,
            &serde_json::to_vec(&inventory).unwrap(),
        ).is_err());
    }
}
