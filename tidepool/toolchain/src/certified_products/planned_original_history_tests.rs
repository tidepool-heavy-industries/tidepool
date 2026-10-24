use super::*;

use crate::artifact_inventory::CanonicalProducerIdentity;
use crate::declaration_context::{
    ExactCompilationRequest, ExactDeclarationContext, ExactSourceAdmission,
};
use crate::declaration_join::NativeAuthoredDeclarationAdmission;
use crate::recovery_artifacts::CertifiedRecoveryProduct;
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use std::cell::RefCell;
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

pub(super) struct PendingOriginal {
    pub(super) request: ExactCompilationRequest,
    pub(super) source: ExactSourceAdmission,
    pub(super) certified: PendingProducts,
}

pub(super) struct IssuedOriginal {
    pub(super) request: ExactCompilationRequest,
    pub(super) source: ExactSourceAdmission,
    pub(super) certified: CapturedProducts,
}

impl PendingOriginal {
    pub(super) fn complete_native_availability(self, producer: &[u8]) -> IssuedOriginal {
        let certified = crate::declaration_context::issue_certified_product_native_availability(
            CanonicalProducerIdentity::from_producer_bytes(producer).sha256(),
            self.certified,
            Some(self.request.compiler_inputs().artifacts()),
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
        .unwrap();
        IssuedOriginal {
            request: self.request,
            source: self.source,
            certified,
        }
    }
}

impl IssuedOriginal {
    pub(super) fn completed_output(&self) -> &crate::declaration_context::IssuedOriginalOutput {
        self.certified.captured_stage().output()
    }
}

fn planned_interface(module: &str) -> Vec<u8> {
    format!("planned-interface:main:{module}").into_bytes()
}

fn assert_canonical_materialization_premises(context: &ExactDeclarationContext) {
    let products = context.recovery_products();
    let mut interfaces = BTreeMap::new();
    for product in &products {
        let interface = product.module_interface();
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

fn planned_product_sidecar(module: &str, body_tag: u8, ordinals: &[u32]) -> Vec<u8> {
    let groups = ordinals
        .iter()
        .copied()
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

pub(super) fn issue_planned_original_with_retained_child(
    root: &Path,
    context: &ExactDeclarationContext,
    module: SessionModule,
    producer: &[u8],
    include: &[PathBuf],
    defined_occurrence: &str,
    child_product: &CertifiedRecoveryProduct,
    child_occurrence: &str,
) -> Result<PendingOriginal, crate::CompileError> {
    assert!(child_product
        .original_native()
        .matches_original(child_product));
    assert_eq!(
        child_product.module_interface().interface_sha256(),
        child_product.owner().skinny_iface_sha256
    );
    let child = child_product.owner();
    assert!(context
        .recovery_products()
        .iter()
        .any(|retained| retained.owner() == child
            && retained.module_interface() == child_product.module_interface()));
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
    // sourceEvidenceGraph carries retained exact imports in the owner receipt;
    // its fresh dependency graph contains only this newly authored module.
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
        value_text("4"),
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
        value_array([value_array([
            value_text("none"),
            value_text(&child.module),
            Value::Bool(false),
            value_text(&child.unit),
            Value::Integer(1.into()),
        ])]),
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
    let home_imports = source_admission.consumed_home_adjacency();
    assert_eq!(source_admission.evidence().modules.len(), 1);
    assert_eq!(
        home_imports[&crate::declaration_join::ExactModuleIdentity {
            unit: "main".into(),
            module: module_name.clone(),
        }],
        vec![crate::declaration_join::ExactModuleIdentity {
            unit: child.unit.clone(),
            module: child.module.clone(),
        }],
    );
    let authored =
        NativeAuthoredDeclarationAdmission::from_planned(&module, &source_admission).unwrap();
    let exact = (&request).product_admission(&source_admission);
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
        package_products: Vec::new(),
        package_links: PackageLinks::empty(),
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
        &crate::artifacts::SourceEvidenceAdmission::from_exact(
            (&exact)
                .source()
                .match_product(
                    (&exact).physical_request().unwrap(),
                    &input,
                    &source,
                    &evidence_bytes,
                    (&parsed).operation(),
                )
                .unwrap(),
        ),
        root,
        producer,
        include,
        Some(&authored),
    )
    .map_err(|error| crate::CompileError::CompilerEvidence(Box::new(error)))?;
    Ok(PendingOriginal {
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
    .complete_native_availability(producer)
}

pub(super) fn issue_planned_original_with_source_value(
    root: &Path,
    module: SessionModule,
    producer: &[u8],
    include: &[PathBuf],
    source_tag: u8,
    body_tag: u8,
    source_value: u8,
) -> PendingOriginal {
    issue_planned_original_with_groups(
        root,
        module,
        producer,
        include,
        source_tag,
        body_tag,
        source_value,
        &[7, 11],
    )
}

fn issue_planned_original_with_groups(
    root: &Path,
    module: SessionModule,
    producer: &[u8],
    include: &[PathBuf],
    source_tag: u8,
    body_tag: u8,
    source_value: u8,
    ordinals: &[u32],
) -> PendingOriginal {
    let module_name = module.module_name();
    let mut source = format!("module {module_name} where\n");
    for ordinal in ordinals {
        let value = if *ordinal == 7 { source_value } else { 4 };
        source.push_str(&format!("entry_{ordinal} = {value}\n"));
    }
    let source_root = root.join(format!("source-{source_tag}"));
    std::fs::create_dir(&source_root).unwrap();
    let input = source_root.join("module.hs");
    std::fs::write(&input, &source).unwrap();
    let mut admitted = evidence(&source);
    admitted.modules[0].module = module_name.clone();
    let canonical_producer = CanonicalProducerIdentity::from_producer_bytes(producer).sha256();
    let context = Arc::new(
        ExactDeclarationContext::from_authenticated_interfaces(
            canonical_producer,
            &crate::artifact_inventory::ArtifactInventory::default().empty_view(),
            crate::artifact_inventory::CompilerInputProjection::default(),
        )
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
        value_text("4"),
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
        Value::Array(vec![]),
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
    let exact = (&request).product_admission(&source_admission);
    let bytes = planned_product_sidecar(&module_name, body_tag, ordinals);
    let package_bytes =
        empty_package_bundle_for_interface(&module_name, &planned_interface(&module_name));
    let parsed = ParsedModuleProducts::decode(&bytes, &package_bytes).unwrap();
    let mut accepted = receipt(&bytes, &admitted, &source);
    accepted.module = module_name.clone();
    accepted.skinny_iface_sha256 = sha(&planned_interface(&module_name));
    accepted.groups = ordinals
        .iter()
        .copied()
        .map(|original_ordinal| AcceptedGroup {
            original_ordinal,
            globals: vec![],
        })
        .collect();
    accepted.dependency_witness_sha256 = sha(&evidence_bytes);
    let packet = CertifiedReceipt {
        package_products: Vec::new(),
        package_links: PackageLinks::empty(),
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
        &crate::artifacts::SourceEvidenceAdmission::from_exact(
            (&exact)
                .source()
                .match_product(
                    (&exact).physical_request().unwrap(),
                    &input,
                    &source,
                    &evidence_bytes,
                    (&parsed).operation(),
                )
                .unwrap(),
        ),
        root,
        producer,
        include,
        Some(&authored),
    )
    .unwrap();
    assert_eq!(
        certified.data().recovery_products[0]
            .module_interface()
            .origin(),
        CanonicalOrigin::NativeAuthoredDeclaration {
            generation: module.gen.0
        }
    );
    PendingOriginal {
        request,
        source: source_admission,
        certified,
    }
}

#[test]
fn raw_original_rebinding_refuses_each_exact_unearned_group() {
    use crate::artifact_inventory::{
        ArtifactEntry, ArtifactInventoryFailure, NativeAdmissionRefusal,
    };
    for original_ordinal in [7, 11] {
        let root = tempfile::tempdir().unwrap();
        let producer = [0x35; 32];
        let canonical_producer = CanonicalProducerIdentity::from_producer_bytes(&producer).sha256();
        let include = [root.path().to_path_buf()];
        let issued = issue_planned_original_with_groups(
            root.path(),
            SessionModule::lib(Generation(7)),
            &producer,
            &include,
            1,
            1,
            3,
            &[original_ordinal],
        )
        .complete_native_availability(&producer);
        let selection = issued.certified.source_selection();
        let product = &issued.certified.data().recovery_products[0];
        let expected_original = ArtifactEntry::original_artifact_id(canonical_producer, product);
        let expected_canonical = ArtifactEntry::canonical_artifact_id(product.module_interface());
        assert_eq!(
            product
                .original_native()
                .groups
                .iter()
                .map(|group| group.group.original_ordinal())
                .collect::<Vec<_>>(),
            vec![original_ordinal]
        );
        let selected = selection
            .selected_original_closure(issued.completed_output().artifacts())
            .unwrap();
        assert_eq!(selected.products().len(), 1);
        assert_eq!(selected.products()[0].owner(), product.owner());
        let sibling = issue_planned_original_with_groups(
            root.path(),
            SessionModule::lib(Generation(7)),
            &producer,
            &include,
            2,
            2,
            3,
            &[original_ordinal],
        )
        .complete_native_availability(&producer);
        let raw = crate::declaration_context::certified_product_artifact_view(
            canonical_producer,
            &[
                product.clone(),
                sibling.certified.data().recovery_products[0].clone(),
            ],
            &[],
            None,
        )
        .unwrap();
        let raw_graph = raw.capture_graph_selection();
        let raw_binding = |id| {
            raw_graph
                .namespace
                .iter()
                .find(|binding| binding.artifact == id)
                .copied()
        };
        assert!(raw_binding(expected_original).is_some());
        assert!(raw_binding(expected_canonical).is_some());
        let key = crate::artifact_inventory::NativeGroupKey {
            artifact: expected_original,
            original_ordinal,
        };
        let NativeGroupCandidates::IssuedAvailability(candidates) = selection
            .native_group_candidates(key, product.owner())
            .unwrap()
        else {
            panic!("the completed singleton retains its issued group");
        };
        assert_eq!(candidates.len(), 1);
        let candidate = *candidates.first().unwrap();
        let Some(CertifiedNativeGroupSupport::Issued(offer)) =
            selection.native_group_support(key, product.owner(), candidate)
        else {
            panic!("the singleton retains its exact issuer");
        };
        assert!(offer.matches_direct_canonical_binding(
            expected_original,
            expected_canonical,
            issued.completed_output().artifacts()
        ));
        assert!(!offer.matches_direct_canonical_binding(
            expected_original,
            expected_canonical,
            &raw
        ));
        assert!(!raw
            .capture_graph_selection()
            .native_groups
            .contains(&candidate));
        let projection = selection.compiler_projection(&raw).unwrap();
        assert_eq!(
            projection
                .snapshot_roles()
                .values()
                .filter_map(crate::artifact_inventory::CompilerInputRole::original)
                .collect::<Vec<_>>(),
            vec![expected_original]
        );
        let result = selection.selected_original_closure(&raw);
        assert!(
            matches!(result, Err(CertificationError::OriginalClosure(ref error))
                if matches!(error.as_ref(), crate::CompileError::ArtifactInventory(error)
                    if matches!(error.failure,
                        ArtifactInventoryFailure::NativeGroupUnavailable {
                            artifact, original_ordinal: refused_ordinal,
                        } if artifact == expected_original && refused_ordinal == original_ordinal)
                    && matches!(&error.native_refusal,
                        Some(NativeAdmissionRefusal::InputNoCompatibleCandidate {
                            selected_original, canonical, selected_canonical,
                        }) if *selected_original == raw_binding(expected_original)
                            && *canonical == expected_canonical
                            && *selected_canonical == raw_binding(expected_canonical)))),
            "raw singleton group {original_ordinal} must retain its exact issuer refusal: {result:?}"
        );
        let retained = issued
            .completed_output()
            .artifacts()
            .retain_custody(&raw)
            .unwrap();
        let selected = selection.selected_original_closure(&retained).unwrap();
        assert_eq!(selected.products().len(), 1);
        assert_eq!(selected.products()[0].owner(), product.owner());
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
        .flat_map(|issued| issued.certified.data().recovery_products.iter().cloned())
        .collect::<Vec<CertifiedRecoveryProduct>>();
    let all_owners = custody
        .iter()
        .map(|product| product.owner().clone())
        .collect::<BTreeSet<_>>();
    let selected_owner = current.certified.data().recovery_products[0]
        .owner()
        .clone();
    assert_eq!(all_owners.len(), 3);
    assert!(all_owners
        .iter()
        .all(|owner| { owner.unit == "main" && owner.module == module_name }));
    let canonical_interface = history[0].certified.data().recovery_products[0].module_interface();
    assert!(history.iter().all(|issued| {
        issued.certified.data().recovery_products[0].module_interface() == canonical_interface
    }));

    let artifact = crate::artifact_inventory::ArtifactEntry::original_artifact_id(
        CanonicalProducerIdentity::from_producer_bytes(&producer).sha256(),
        &current.certified.data().recovery_products[0],
    );
    let required = original_available_groups(&current.certified.data().recovery_products[0])
        .unwrap()
        .map(|group| group.original_ordinal())
        .collect::<BTreeSet<_>>();
    assert_eq!(required, BTreeSet::from([7, 11]));
    let rebound = crate::declaration_context::certified_product_artifact_view(
        CanonicalProducerIdentity::from_producer_bytes(&producer).sha256(),
        &custody,
        &[],
        None,
    )
    .unwrap();
    let mut deliberately_missing = BTreeSet::new();
    for original_ordinal in required {
        let key = crate::artifact_inventory::NativeGroupKey {
            artifact,
            original_ordinal,
        };
        let NativeGroupCandidates::IssuedAvailability(bindings) = current
            .certified
            .source_selection()
            .native_group_candidates(key, &selected_owner)
            .unwrap()
        else {
            panic!("the physical producer completed this native availability");
        };
        assert!(!bindings.is_empty());
        for binding in &bindings {
            let Some(CertifiedNativeGroupSupport::Issued(offer)) = current
                .certified
                .source_selection()
                .native_group_support(key, &selected_owner, *binding)
            else {
                panic!("completed availability retains its exact issuing offer");
            };
            assert!(offer.group_bindings(key).any(|issued| issued == *binding));
            assert!(
                !rebound
                    .capture_graph_selection()
                    .native_groups
                    .contains(&*binding),
                "equal payloads do not recreate an issued Group binding"
            );
        }
        deliberately_missing.insert(original_ordinal);
    }
    let rebound_result = current
        .certified
        .source_selection()
        .selected_original_closure(&rebound);
    assert!(
        matches!(&rebound_result,
        Err(CertificationError::OriginalClosure(error)) if matches!(error.as_ref(),
            crate::CompileError::ArtifactInventory(error) if matches!(&error.failure,
                crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
                    artifact: refused, original_ordinal,
                } if *refused == artifact && deliberately_missing.contains(original_ordinal)))),
        "the exact current Original must refuse a deliberately missing required group: {:?}",
        rebound_result.as_ref().err(),
    );

    // Retain every version through its actual physical issuer, in both orders.
    let versions = history.iter().collect::<Vec<_>>();
    for ordered in [versions.clone(), versions.iter().rev().copied().collect()] {
        let mut view = current.completed_output().artifacts().clone();
        for version in ordered {
            view = view
                .retain_custody(version.completed_output().artifacts())
                .unwrap();
        }
        assert_eq!(
            view.capture_graph_selection().namespace,
            current
                .completed_output()
                .artifacts()
                .capture_graph_selection()
                .namespace
        );
        assert_eq!(
            view.artifact_ids().into_iter().collect::<BTreeSet<_>>(),
            history
                .iter()
                .flat_map(|version| version.completed_output().artifacts().artifact_ids())
                .collect()
        );
        let originals = current
            .certified
            .source_selection()
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
        let exact = (&current.request).product_admission(&current.source);
        let output = current.completed_output();
        let certified = crate::declaration_join::certify_same_offer_planned_declaration(
            module,
            &format!("module {module_name} where\nentry_7 = 3\nentry_11 = 4\n"),
            &producer,
            &originals,
            &output,
            &exact,
            &include,
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
                &output,
                &exact,
                &include,
                &serde_json::to_vec(&inventory).unwrap(),
            )
            .is_err()
        );
    }
}

#[derive(Default, serde::Serialize)]
struct PlannedHistoryCoverage {
    callbacks: usize,
    completed: usize,
    forward_by_distinct_versions: BTreeMap<usize, usize>,
    reverse_by_distinct_versions: BTreeMap<usize, usize>,
}

#[test]
fn selected_planned_original_matches_generated_issue_and_retention_history() {
    let strategy = (
        (1u8..8, 1u8..8).prop_filter(
            "history must retain distinct older and current versions",
            |(older, current)| older != current,
        ),
        prop::collection::vec(1u8..8, 0..3),
        any::<bool>(),
    );
    let mut config = proptest::test_runner::contextualize_config(planned_history_property_config());
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::selected_planned_original_matches_generated_issue_and_retention_history"
    ));
    let configured_cases = config.cases;
    let configured_max_shrink_iters = config.max_shrink_iters;
    let configuration = format!("{config:?}");
    let mut runner = TestRunner::new(config);
    let coverage = RefCell::new(PlannedHistoryCoverage::default());
    let result = runner.run(
        &strategy,
        |((older, current), repeated, reverse_custody)| {
            coverage.borrow_mut().callbacks += 1;
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
                    root.path(),
                    module,
                    &producer,
                    &include,
                    source_tag as u8,
                    *tag,
                );
                issued_by_tag.insert(*tag, issued);
            }
            let selected_issue = issued_by_tag.get(&current).unwrap();
            let selected_owner = selected_issue.certified.data().recovery_products[0]
                .owner()
                .clone();
            let mut custody = issued_by_tag
                .values()
                .map(|issued| issued.certified.data().recovery_products[0].clone())
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
            let mut versions = issued_by_tag.values().collect::<Vec<_>>();
            if reverse_custody {
                versions.reverse();
            }
            let mut view = selected_issue.completed_output().artifacts().clone();
            for version in versions {
                view = view
                    .retain_custody(version.completed_output().artifacts())
                    .unwrap();
            }
            prop_assert_eq!(
                view.capture_graph_selection().namespace,
                selected_issue
                    .completed_output()
                    .artifacts()
                    .capture_graph_selection()
                    .namespace
            );
            prop_assert_eq!(
                view.artifact_ids().into_iter().collect::<BTreeSet<_>>(),
                issued_by_tag
                    .values()
                    .flat_map(|version| version.completed_output().artifacts().artifact_ids())
                    .collect()
            );
            let selected = selected_issue
                .certified
                .source_selection()
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
            let exact = (&selected_issue.request).product_admission(&selected_issue.source);
            let source = format!("module {module_name} where\nentry_7 = 3\nentry_11 = 4\n");
            let output = selected_issue.completed_output();
            let joined = crate::declaration_join::certify_same_offer_planned_declaration(
                module,
                &source,
                &producer,
                &selected,
                &output,
                &exact,
                &include,
                &serde_json::to_vec(&inventory).unwrap(),
            );
            prop_assert!(joined.is_ok());
            let drifted = format!("module {module_name} where\nentry_7 = 5\nentry_11 = 4\n");
            prop_assert!(
                crate::declaration_join::certify_same_offer_planned_declaration(
                    module,
                    &drifted,
                    &producer,
                    &selected,
                    &output,
                    &exact,
                    &include,
                    &serde_json::to_vec(&inventory).unwrap(),
                )
                .is_err()
            );
            let mut observed = coverage.borrow_mut();
            observed.completed += 1;
            let partitions = if reverse_custody {
                &mut observed.reverse_by_distinct_versions
            } else {
                &mut observed.forward_by_distinct_versions
            };
            *partitions.entry(model_versions.len()).or_default() += 1;
            Ok(())
        },
    );
    eprintln!(
        "planned_original_history_campaign={}",
        serde_json::json!({
            "configured_cases": configured_cases,
            "configured_max_shrink_iters": configured_max_shrink_iters,
            "configuration": configuration,
            "observation_scope": "runner callbacks in this process, including replay and shrinking",
            "observed": &*coverage.borrow(),
        })
    );
    if let Err(error) = result {
        panic!("planned original history property failed: {error}");
    }
}
