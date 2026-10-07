use super::*;

use crate::artifact_inventory::CanonicalProducerIdentity;
use crate::cache::{ImportQualifier, ModuleEvidence, ModuleImportEvidence, ResolutionEvidence};
use crate::declaration_context::{ExactDeclarationContext, ExactProductAdmission};
use tidepool_repr::execution_schema::{Group, HeapRhs};

const UNIT: &str = "main";
const A: &str = "A";
const B: &str = "B";
const A_ORDINAL: u32 = 7;
const B_ORDINAL: u32 = 11;

fn binder(module: &str, ordinal: u32) -> SymbolIdentity {
    SymbolIdentity {
        unit: UNIT.into(),
        module: module.into(),
        namespace: "value".into(),
        occurrence: format!("entry_{ordinal}"),
        record_parent: None,
    }
}

fn package_bundle() -> Vec<u8> {
    let roots = |module: &str| {
        let value = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text(UNIT),
                value_text(module),
                value_text(hex(&sha(&[0x42]))),
            ]),
            value_array([]),
            value_array([]),
        ]);
        receipt_bytes(&value)
    };
    let value = value_array([
        value_text("TPPKGBUNDLES"),
        Value::Integer(1.into()),
        value_array([
            value_array([value_text(UNIT), value_text(A), Value::Bytes(roots(A))]),
            value_array([value_text(UNIT), value_text(B), Value::Bytes(roots(B))]),
        ]),
    ]);
    receipt_bytes(&value)
}

/// Produces raw compiler-shaped sidecar bytes only. `certify_products` remains
/// the sole issuer of every owner and original native witness used below.
fn raw_products(b_body: u8) -> Vec<u8> {
    let b_binder = binder(B, B_ORDINAL);
    let module = |name: &str, ordinal: u32, body: Vec<u8>, imports: bool| {
        let mut wire = testing::wire_program();
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = binder(name, ordinal);
        top.binding.rhs = HeapRhs::Bytes(body);
        wire.globals.clear();
        if imports {
            wire.globals.push(GlobalDecl {
                identity: b_binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            });
        }
        let top = top.clone();
        wire.bindings = vec![Group::Recursive(vec![top])];
        wire.expressions.nodes.clear();
        testing::projected_group(wire, ordinal).unwrap()
    };
    let mut products = Vec::new();
    for ordinal in [A_ORDINAL, 8] {
        products.push(module(A, ordinal, vec![0xA, ordinal as u8], true));
    }
    for ordinal in [B_ORDINAL, 12] {
        products.push(module(B, ordinal, vec![0xB, ordinal as u8, b_body], false));
    }
    tidepool_test_data::prepared_encode::encode_module_products(&[
        RawModuleProduct {
            unit: UNIT.into(),
            module: A.into(),
            interface: vec![0x42],
            groups: products[..2].to_vec(),
        },
        RawModuleProduct {
            unit: UNIT.into(),
            module: B.into(),
            interface: vec![0x42],
            groups: products[2..].to_vec(),
        },
    ])
}

fn source_evidence(root: &Path, target_source: &str, b_body: u8) -> DependencyEvidence {
    let a_path = root.join("A.hs");
    let b_path = root.join("B.hs");
    let a_source = "module A where\nimport B\na = entry_11\n";
    let b_source = format!("module B where\nb = {}\n", b_body);
    std::fs::write(&a_path, a_source).unwrap();
    std::fs::write(&b_path, &b_source).unwrap();
    let mut evidence = evidence(target_source);
    evidence.sources[0] = SourceEvidence {
        path: a_path.clone(),
        sha256: hex(&sha(a_source.as_bytes())),
    };
    evidence.sources.push(SourceEvidence {
        path: b_path.clone(),
        sha256: hex(&sha(b_source.as_bytes())),
    });
    evidence.modules = vec![
        ModuleEvidence {
            unit: UNIT.into(),
            module: A.into(),
            boot: false,
            source: a_path.clone(),
            imports: vec![ModuleImportEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: B.into(),
                boot: false,
                selected: Some(b_path.clone()),
            }],
            product: ProductAvailability::Ready,
        },
        ModuleEvidence {
            unit: UNIT.into(),
            module: B.into(),
            boot: false,
            source: b_path.clone(),
            imports: vec![],
            product: ProductAvailability::Ready,
        },
    ];
    evidence.resolutions = vec![ResolutionEvidence {
        qualifier: ImportQualifier::Unqualified,
        module: B.into(),
        boot: false,
        selected: Some(b_path.clone()),
        candidates: vec![b_path],
    }];
    evidence
}

fn ordinary_receipts(
    bytes: &[u8],
    parsed: &ParsedModuleProducts,
    evidence: &DependencyEvidence,
) -> Vec<CertifiedModuleReceipt> {
    parsed
        .products()
        .iter()
        .map(|product| {
            let module = &product.module;
            let source = evidence
                .sources
                .iter()
                .find(|source| {
                    source
                        .path
                        .file_stem()
                        .is_some_and(|stem| stem.to_string_lossy().as_ref() == module.as_str())
                })
                .unwrap();
            let mut accepted = receipt(bytes, evidence, "");
            accepted.module = module.clone();
            accepted.unit = product.unit.clone();
            accepted.source_sha256 = sha(std::fs::read_to_string(&source.path).unwrap().as_bytes());
            accepted.dependency_witness_sha256 = sha(&serde_json::to_vec(evidence).unwrap());
            accepted.groups = product
                .groups
                .iter()
                .map(|group| {
                    let globals = if module == A {
                        vec![AcceptedGlobal {
                            identity: binder(B, B_ORDINAL),
                            rep: RuntimeRep::LiftedRef,
                            entry_signature: None,
                            required_evaluated: false,
                            owner: ReceiptImportOwner::Source {
                                unit: UNIT.into(),
                                module: B.into(),
                                module_version: None,
                                original_ordinal: B_ORDINAL,
                                binder: binder(B, B_ORDINAL),
                            },
                        }]
                    } else {
                        vec![]
                    };
                    AcceptedGroup {
                        original_ordinal: group.original_ordinal(),
                        globals,
                    }
                })
                .collect();
            if module == A {
                accepted
                    .interface_requirements
                    .insert((UNIT.into(), B.into()), sha(&[0x42]));
            }
            accepted
        })
        .collect()
}

fn expected_promoted_versions(
    parsed: &ParsedModuleProducts,
    canonical: &BTreeMap<String, CertifiedModuleInterface>,
) -> BTreeMap<String, ModuleVersion> {
    assert_eq!(parsed.products().len(), 2);
    assert_eq!(
        parsed.products()[0]
            .groups
            .iter()
            .map(|g| g.original_ordinal())
            .collect::<Vec<_>>(),
        [7, 8]
    );
    assert_eq!(
        parsed.products()[1]
            .groups
            .iter()
            .map(|g| g.original_ordinal())
            .collect::<Vec<_>>(),
        [11, 12]
    );
    let mut nodes = BTreeMap::new();
    for (index, product) in parsed.products().iter().enumerate() {
        let package =
            &parsed.package_imports.as_ref().unwrap()[&(UNIT.into(), product.module.clone())];
        let ordinals: &[u32] = if product.module == A {
            &[7, 8]
        } else {
            &[11, 12]
        };
        let groups = ordinals
            .iter()
            .map(|ordinal| {
                let imports = if product.module == A {
                    vec![value_array([
                        value_text("local-source"),
                        value_text(UNIT),
                        value_text(B),
                        Value::Integer(11.into()),
                        value_array([
                            value_text("main"),
                            value_text("B"),
                            value_text("value"),
                            value_text("entry_11"),
                            Value::Null,
                        ]),
                    ])]
                } else {
                    vec![]
                };
                value_array([Value::Integer((*ordinal).into()), value_array(imports)])
            })
            .collect::<Vec<_>>();
        nodes.insert(
            product.module.clone(),
            value_array([
                value_text(UNIT),
                value_text(&product.module),
                value_text(hex(&sha(canonical[&product.module].certificate_bytes()))),
                value_text(hex(&sha(&parsed.sidecars[index]))),
                value_text(hex(&sha(package))),
                value_array(groups),
            ]),
        );
    }
    [A, B]
        .into_iter()
        .map(|root| {
            let reachable = if root == A { vec![A, B] } else { vec![B] };
            let graph = value_array(reachable.into_iter().map(|name| nodes[name].clone()));
            let graph_bytes = receipt_bytes(&graph);
            let mut digest = Sha256::new();
            for field in [
                b"retained-core-home-v1".as_slice(),
                UNIT.as_bytes(),
                root.as_bytes(),
                graph_bytes.as_slice(),
            ] {
                digest.update((field.len() as u64).to_be_bytes());
                digest.update(field);
            }
            (root.into(), ModuleVersion(digest.finalize().into()))
        })
        .collect()
}

fn native_import_facts(
    product: &crate::recovery_artifacts::CertifiedRecoveryProduct,
) -> Vec<Vec<PendingImportOwner>> {
    product
        .original_native()
        .unwrap()
        .groups
        .iter()
        .map(|group| group.imports.to_vec())
        .collect()
}

fn certify_promotion(
    context: &ExactDeclarationContext,
    index: usize,
    product_bytes: &[u8],
    parsed: &ParsedModuleProducts,
    packet: &CertifiedReceipt,
    evidence: &DependencyEvidence,
    target_source: &str,
    producer: &[u8; 32],
    include: &[PathBuf],
    root: &Path,
) -> CertifiedProducts {
    let projection = context.compiler_input_projection().interface_only();
    let context = Arc::new(
        context
            .clone()
            .with_compiler_input_projection(projection)
            .unwrap(),
    );
    let directory = root.join(format!("promotion-{index}"));
    std::fs::create_dir(&directory).unwrap();
    let input = directory.join("Target.hs");
    std::fs::write(&input, target_source).unwrap();
    let request = context
        .prepare_compilation(&directory.join("inputs"), producer)
        .unwrap();
    assert!(request.compiler_original_products().unwrap().is_empty());
    let mut worker = evidence.clone();
    worker.sources.last_mut().unwrap().path = input.clone();
    let complete_evidence = serde_json::to_vec(&worker).unwrap();
    let receipt_root = directory.join(".exact-compilations/current");
    std::fs::create_dir_all(&receipt_root).unwrap();
    let snapshot = receipt_root.join("source.hs");
    std::fs::write(&snapshot, target_source).unwrap();
    let exact_receipt = value_array([
        value_text("TPEXACTCOMPILE"),
        value_text("3"),
        value_text(&request.request_sha256),
        value_text(hex(&request.semantic_sha256)),
        value_text(input.to_string_lossy()),
        value_text(hex(&sha(target_source.as_bytes()))),
        value_text(snapshot.to_string_lossy()),
        value_text(String::from_utf8(complete_evidence).unwrap()),
        value_array([
            value_array([
                value_text(UNIT),
                value_text(A),
                Value::Bool(false),
                value_array([]),
            ]),
            value_array([
                value_text(UNIT),
                value_text(B),
                Value::Bool(false),
                value_array([]),
            ]),
        ]),
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
        .admit_source(&input, target_source, &evidence_bytes)
        .unwrap();
    let exact = ExactProductAdmission {
        request: &request,
        source: &source_admission,
    };
    let mut packet = packet.clone();
    for module in &mut packet.modules {
        module.product_sha256 = sha(product_bytes);
    }
    certify_products(
        None,
        &packet,
        parsed,
        &evidence_bytes,
        &input,
        root,
        &CompletedSourceEvidence::from_normalized(evidence.clone(), target_source).unwrap(),
        target_source,
        producer,
        include,
        Some(&exact),
        None,
    )
    .unwrap()
}

#[test]
fn retained_core_promotion_rekeys_nonempty_source_import_history() {
    let root = tempfile::tempdir().unwrap();
    let target_source = "module Target where\n";
    let producer = [31; 32];
    let canonical_producer = CanonicalProducerIdentity::from_producer_bytes(&producer).sha256();
    let include = [root.path().to_path_buf()];

    let initial_bytes = raw_products(0);
    let initial_evidence = source_evidence(root.path(), target_source, 0);
    let initial_input = root.path().join("Target.hs");
    std::fs::write(&initial_input, target_source).unwrap();
    let mut initial_worker = initial_evidence.clone();
    initial_worker.sources.push(SourceEvidence {
        path: "@generated-source".into(),
        sha256: hex(&sha(target_source.as_bytes())),
    });
    let initial_evidence_bytes = serde_json::to_vec(&initial_worker).unwrap();
    let normalized =
        CompletedSourceEvidence::from_normalized(initial_worker.clone(), target_source).unwrap();
    let parsed = ParsedModuleProducts::decode(&initial_bytes, &package_bundle()).unwrap();
    assert_eq!(parsed.products().len(), 2);
    let accepted = ordinary_receipts(&initial_bytes, &parsed, &initial_worker);
    let packet = CertifiedReceipt {
        source_recipe: WorkerExecutionSource::Ordinary,
        finalization: fixture_finalization(Some(root.path()), &accepted),
        modules: accepted,
        targets: BTreeMap::new(),
        packages: BTreeMap::new(),
    };
    let initial = certify_products(
        None,
        &packet,
        &parsed,
        &initial_evidence_bytes,
        &initial_input,
        root.path(),
        &normalized,
        target_source,
        &producer,
        &include,
        None,
        None,
    )
    .unwrap();
    let prior = initial.recovery_products.clone();
    assert_eq!(prior.len(), 2);
    let initial_a = prior.iter().find(|p| p.owner().module == A).unwrap();
    let initial_b = prior.iter().find(|p| p.owner().module == B).unwrap();
    let canonical = prior
        .iter()
        .map(|product| {
            (
                product.owner().module.clone(),
                product.module_interface().unwrap().clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(initial_a.original_native().unwrap().groups.len(), 2);
    assert_eq!(initial_b.original_native().unwrap().groups.len(), 2);
    assert!(initial_a.original_native().unwrap().groups.iter().all(|g| {
        matches!(&g.imports[..], [PendingImportOwner::Source { owner, original_ordinal, binder: imported }] if owner == initial_b.owner() && *original_ordinal == B_ORDINAL && imported == &binder(B, B_ORDINAL))
    }));
    let mut context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_checked_original_products(canonical_producer, &prior)
        .unwrap();

    // The exact request exposes canonical interfaces while withholding native
    // originals from the compiler input.
    let projection = context.compiler_input_projection().interface_only();
    context = context.with_compiler_input_projection(projection).unwrap();
    let directory = root.path().join("promotion-0");
    std::fs::create_dir(&directory).unwrap();
    let input = directory.join("Target.hs");
    std::fs::write(&input, target_source).unwrap();
    let request = Arc::new(context.clone())
        .prepare_compilation(&directory.join("inputs"), &producer)
        .unwrap();
    assert!(request.compiler_original_products().unwrap().is_empty());
    let mut worker = initial_worker.clone();
    worker.sources.last_mut().unwrap().path = input.clone();
    let complete_evidence = serde_json::to_vec(&worker).unwrap();
    let receipt_root = directory.join(".exact-compilations/current");
    std::fs::create_dir_all(&receipt_root).unwrap();
    let snapshot = receipt_root.join("source.hs");
    std::fs::write(&snapshot, target_source).unwrap();
    let exact_receipt = value_array([
        value_text("TPEXACTCOMPILE"),
        value_text("3"),
        value_text(&request.request_sha256),
        value_text(hex(&request.semantic_sha256)),
        value_text(input.to_string_lossy()),
        value_text(hex(&sha(target_source.as_bytes()))),
        value_text(snapshot.to_string_lossy()),
        value_text(String::from_utf8(complete_evidence.clone()).unwrap()),
        value_array([
            value_array([
                value_text(UNIT),
                value_text(A),
                Value::Bool(false),
                value_array([]),
            ]),
            value_array([
                value_text(UNIT),
                value_text(B),
                Value::Bool(false),
                value_array([]),
            ]),
        ]),
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
        .admit_source(&input, target_source, &evidence_bytes)
        .unwrap();
    let exact = ExactProductAdmission {
        request: &request,
        source: &source_admission,
    };

    let promoted_bytes = raw_products(0);
    let promoted = ParsedModuleProducts::decode(&promoted_bytes, &package_bundle()).unwrap();
    let promoted_receipts = promoted
        .products()
        .iter()
        .map(|product| {
            let original = prior
                .iter()
                .find(|old| old.owner().module == product.module)
                .unwrap();
            let canonical = original.module_interface().unwrap();
            let mut accepted = receipt(&promoted_bytes, &worker, "");
            accepted.origin = ProductOrigin::RetainedCore;
            accepted.unit = UNIT.into();
            accepted.module = product.module.clone();
            accepted.module_version = None;
            accepted.product_sha256 = sha(&promoted_bytes);
            accepted.source_sha256 = canonical.source_sha256();
            accepted.dependency_witness_sha256 = sha(canonical.certificate_bytes());
            accepted.interface_requirements = canonical.requirements().clone();
            accepted.groups = product
                .groups
                .iter()
                .map(|group| AcceptedGroup {
                    original_ordinal: group.original_ordinal(),
                    globals: if product.module == A {
                        vec![AcceptedGlobal {
                            identity: binder(B, B_ORDINAL),
                            rep: RuntimeRep::LiftedRef,
                            entry_signature: None,
                            required_evaluated: false,
                            owner: ReceiptImportOwner::Source {
                                unit: UNIT.into(),
                                module: B.into(),
                                module_version: None,
                                original_ordinal: B_ORDINAL,
                                binder: binder(B, B_ORDINAL),
                            },
                        }]
                    } else {
                        vec![]
                    },
                })
                .collect();
            accepted
        })
        .collect::<Vec<_>>();
    let promoted_packet = CertifiedReceipt {
        source_recipe: WorkerExecutionSource::ExactUnavailable(
            SourceRecipeUnavailable::NoFreshOriginals,
        ),
        finalization: fixture_finalization(None, &promoted_receipts),
        modules: promoted_receipts,
        targets: BTreeMap::new(),
        packages: BTreeMap::new(),
    };
    let certified = certify_products(
        None,
        &promoted_packet,
        &promoted,
        &evidence_bytes,
        &input,
        root.path(),
        &normalized,
        target_source,
        &producer,
        &include,
        Some(&exact),
        None,
    )
    .unwrap();
    let selected_owners = certified
        .source_selection
        .selected_original_owners()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(selected_owners.len(), 2);
    assert_eq!(
        selected_owners
            .iter()
            .map(|owner| owner.module.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([A, B])
    );
    let current_a = certified
        .recovery_products
        .iter()
        .find(|p| selected_owners.contains(p.owner()) && p.owner().module == A)
        .unwrap();
    let current_b = certified
        .recovery_products
        .iter()
        .find(|p| selected_owners.contains(p.owner()) && p.owner().module == B)
        .unwrap();
    assert_eq!(current_a.original_native().unwrap().groups.len(), 2);
    assert_eq!(
        current_a
            .original_native()
            .unwrap()
            .groups
            .iter()
            .map(|group| group.group().original_ordinal())
            .collect::<Vec<_>>(),
        [7, 8]
    );
    assert_eq!(
        current_b
            .original_native()
            .unwrap()
            .groups
            .iter()
            .map(|group| group.group().original_ordinal())
            .collect::<Vec<_>>(),
        [11, 12]
    );
    let expected_versions = expected_promoted_versions(&promoted, &canonical);
    assert_eq!(current_a.owner().module_version, expected_versions[A]);
    assert_eq!(current_b.owner().module_version, expected_versions[B]);
    assert!(current_a.original_native().unwrap().groups.iter().all(|g| {
        matches!(&g.imports[..], [PendingImportOwner::Source { owner, original_ordinal, binder: imported }] if owner == current_b.owner() && *original_ordinal == B_ORDINAL && imported == &binder(B, B_ORDINAL))
    }));

    let mut changed_import = promoted_packet.clone();
    let a_receipt = changed_import
        .modules
        .iter_mut()
        .find(|module| module.module == A)
        .unwrap();
    let ReceiptImportOwner::Source { module_version, .. } =
        &mut a_receipt.groups[0].globals[0].owner
    else {
        unreachable!()
    };
    *module_version = Some(initial_b.owner().module_version.clone());
    assert!(certify_products(
        None,
        &changed_import,
        &promoted,
        &evidence_bytes,
        &input,
        root.path(),
        &normalized,
        target_source,
        &producer,
        &include,
        Some(&exact),
        None,
    )
    .is_err());

    let mut duplicate = promoted_packet.clone();
    let a_receipt = duplicate
        .modules
        .iter_mut()
        .find(|module| module.module == A)
        .unwrap();
    a_receipt.groups.push(a_receipt.groups[0].clone());
    assert!(certify_products(
        None,
        &duplicate,
        &promoted,
        &evidence_bytes,
        &input,
        root.path(),
        &normalized,
        target_source,
        &producer,
        &include,
        Some(&exact),
        None,
    )
    .is_err());

    let first_a_owner = current_a.owner().clone();
    let first_b_owner = current_b.owner().clone();
    let mut owner_history = prior
        .iter()
        .map(|product| product.owner().clone())
        .collect::<BTreeSet<_>>();
    let mut historical_a_imports = prior
        .iter()
        .filter(|product| product.owner().module == A)
        .map(|product| (product.owner().clone(), native_import_facts(product)))
        .collect::<BTreeMap<_, _>>();
    historical_a_imports.insert(current_a.owner().clone(), native_import_facts(current_a));
    owner_history.insert(current_a.owner().clone());
    owner_history.insert(current_b.owner().clone());
    assert_eq!(
        certified
            .recovery_products
            .iter()
            .map(|product| product.owner().clone())
            .collect::<BTreeSet<_>>(),
        owner_history
    );
    assert_eq!(certified.recovery_products.len(), owner_history.len());
    let mut history_context = context
        .extend_checked_original_products(
            canonical_producer,
            &[current_a.clone(), current_b.clone()],
        )
        .unwrap();
    let mut latest_recovery = certified.recovery_products.clone();
    for (index, body_tag) in [0, 1, 0].into_iter().enumerate() {
        let product_bytes = raw_products(body_tag);
        let parsed = ParsedModuleProducts::decode(&product_bytes, &package_bundle()).unwrap();
        let next = certify_promotion(
            &history_context,
            index + 1,
            &product_bytes,
            &parsed,
            &promoted_packet,
            &initial_worker,
            target_source,
            &producer,
            &include,
            root.path(),
        );
        let selected = next
            .source_selection
            .selected_original_owners()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(selected.len(), 2);
        let current_a = next
            .recovery_products
            .iter()
            .find(|p| selected.contains(p.owner()) && p.owner().module == A)
            .unwrap();
        let current_b = next
            .recovery_products
            .iter()
            .find(|p| selected.contains(p.owner()) && p.owner().module == B)
            .unwrap();
        let expected = expected_promoted_versions(&parsed, &canonical);
        assert_eq!(current_a.owner().module_version, expected[A]);
        assert_eq!(current_b.owner().module_version, expected[B]);
        assert_eq!(
            current_a
                .original_native()
                .unwrap()
                .groups
                .iter()
                .map(|group| group.group().original_ordinal())
                .collect::<Vec<_>>(),
            [7, 8]
        );
        assert_eq!(
            current_b
                .original_native()
                .unwrap()
                .groups
                .iter()
                .map(|group| group.group().original_ordinal())
                .collect::<Vec<_>>(),
            [11, 12]
        );
        if body_tag == 0 {
            assert_eq!(current_a.owner(), &first_a_owner);
            assert_eq!(current_b.owner(), &first_b_owner);
        } else {
            assert_ne!(current_a.owner(), &first_a_owner);
            assert_ne!(current_b.owner(), &first_b_owner);
        }
        assert!(current_a.original_native().unwrap().groups.iter().all(|g| {
            matches!(&g.imports[..], [PendingImportOwner::Source { owner, original_ordinal, binder: imported }] if owner == current_b.owner() && *original_ordinal == B_ORDINAL && imported == &binder(B, B_ORDINAL))
        }));
        if let Some(previous) = historical_a_imports.get(current_a.owner()) {
            assert_eq!(previous, &native_import_facts(current_a));
        } else {
            historical_a_imports.insert(current_a.owner().clone(), native_import_facts(current_a));
        }
        owner_history.insert(current_a.owner().clone());
        owner_history.insert(current_b.owner().clone());
        assert_eq!(
            next.recovery_products
                .iter()
                .map(|product| product.owner().clone())
                .collect::<BTreeSet<_>>(),
            owner_history
        );
        assert_eq!(next.recovery_products.len(), owner_history.len());
        history_context = history_context
            .extend_checked_original_products(
                canonical_producer,
                &[current_a.clone(), current_b.clone()],
            )
            .unwrap();
        latest_recovery = next.recovery_products.clone();
    }
    assert_eq!(owner_history.len(), 6);
    let retained_owners = latest_recovery
        .iter()
        .map(|product| product.owner().clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(retained_owners, owner_history);
    assert_eq!(latest_recovery.len(), owner_history.len());
    for product in latest_recovery
        .iter()
        .filter(|product| product.owner().module == A)
    {
        assert_eq!(
            historical_a_imports.get(product.owner()),
            Some(&native_import_facts(product))
        );
        for group in product.original_native().unwrap().groups.iter() {
            assert!(matches!(
                &group.imports[..],
                [PendingImportOwner::Source { owner, original_ordinal, binder: imported }]
                    if owner.module == B
                        && retained_owners.contains(owner)
                        && *original_ordinal == B_ORDINAL
                        && imported == &binder(B, B_ORDINAL)
            ));
        }
    }
    for old in &prior {
        let retained = latest_recovery
            .iter()
            .find(|p| p.owner() == old.owner())
            .unwrap();
        assert!(old
            .original_byte_anchors()
            .iter()
            .zip(retained.original_byte_anchors())
            .all(|(a, b)| Arc::ptr_eq(a, b)));
    }
    let retained_initial_a = latest_recovery
        .iter()
        .find(|product| product.owner() == initial_a.owner())
        .unwrap();
    assert!(retained_initial_a.original_native().unwrap().groups.iter().all(|group| {
        matches!(&group.imports[..], [PendingImportOwner::Source { owner, .. }] if owner == initial_b.owner())
    }));
}
