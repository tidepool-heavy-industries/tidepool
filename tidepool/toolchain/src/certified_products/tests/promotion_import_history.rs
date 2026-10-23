use super::*;

use crate::artifact_inventory::CanonicalProducerIdentity;
use crate::cache::{ImportQualifier, ModuleEvidence, ModuleImportEvidence, ResolutionEvidence};
use crate::declaration_context::ExactDeclarationContext;
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

fn empty_item_selection(stage_id: [u8; 32]) -> PackageSelection {
    empty_package_selection(stage_id, "item")
}

fn package_bundle() -> Vec<u8> {
    let roots = |module: &str| {
        let value = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text(UNIT),
                value_text(module),
                value_text(hex(&sha(module.as_bytes()))),
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
            interface: A.as_bytes().to_vec(),
            groups: products[..2].to_vec(),
        },
        RawModuleProduct {
            unit: UNIT.into(),
            module: B.into(),
            interface: B.as_bytes().to_vec(),
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
            accepted.skinny_iface_sha256 = sha(&product.interface);
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
                    .insert((UNIT.into(), B.into()), sha(B.as_bytes()));
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
) -> Vec<Vec<ReceiptImportOwner>> {
    product
        .original_native()
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
) -> CapturedProducts {
    let projection = context.compiler_input_projection().interface_only();
    let context = Arc::new(
        context
            .clone()
            .recover_fixture_projection(projection)
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
        value_text("4"),
        value_text(&request.request_sha256),
        value_text(hex(&request.semantic_sha256)),
        value_text(input.to_string_lossy()),
        value_text(hex(&sha(target_source.as_bytes()))),
        value_text(snapshot.to_string_lossy()),
        value_text(String::from_utf8(complete_evidence).unwrap()),
        value_array([value_array([
            value_text(UNIT),
            value_text("Target"),
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
        .admit_source(&input, target_source, &evidence_bytes)
        .unwrap();
    let exact = (&request).product_admission(&source_admission);
    let mut packet = packet.clone();
    for module in &mut packet.modules {
        module.product_sha256 = sha(product_bytes);
    }
    let products = certify_products(
        None,
        &packet,
        parsed,
        &crate::artifacts::SourceEvidenceAdmission::from_exact(
            (&exact)
                .source()
                .match_product(
                    (&exact).physical_request().unwrap(),
                    &input,
                    target_source,
                    &evidence_bytes,
                    (parsed).operation(),
                )
                .unwrap(),
        ),
        root,
        producer,
        include,
        None,
    )
    .unwrap();
    crate::declaration_context::issue_certified_product_native_availability(
        CanonicalProducerIdentity::from_producer_bytes(producer).sha256(),
        products,
        Some(request.compiler_inputs().artifacts()),
        &mut PackageInterfaceValidation::with_inventory(Arc::clone(parsed.operation())),
    )
    .unwrap()
}

#[test]
fn retained_core_promotion_rekeys_nonempty_source_import_history() {
    check_actual_retained_core_promotion_history();
}

fn check_actual_retained_core_promotion_history() {
    use crate::artifact_inventory::{ArtifactBindingNode, ArtifactDependency, ArtifactPayload};
    use crate::cell_plan::{WorldCellAdmissionSubject, WorldCellReservationRequest};
    use crate::compiler_world::{
        CapturedCompilerSourceRoot, CapturedCompilerSources, CompilerWorldLexicalSurface,
        VisibleWorldBindings, WorldSourceSelection,
    };
    use tidepool_extract_cmd::{
        settle_compiler_transaction_cancellable, CompilerTransactionCancellation,
        CompilerTransactionClose, CompilerTransactionOrigin,
    };

    let fixture = crate::artifacts::test_support::prepare_source_publication_entry_fixture()
        .expect("ordinary compiler issues the independent pre-promotion products");
    let root_owner = fixture.root.descriptor.owner.clone();
    let helper_owner = fixture.helper.descriptor.owner.clone();
    let original = [&fixture.root, &fixture.helper].map(|entry| {
        let ArtifactPayload::Original(product) = &entry.payload else {
            unreachable!()
        };
        product.clone()
    });
    let original_root_imports = original[0]
        .original_native()
        .groups
        .iter()
        .flat_map(|group| group.imports().iter())
        .filter_map(|import| match import {
            ReceiptImportOwner::Source {
                unit,
                module,
                module_version: Some(version),
                original_ordinal,
                binder,
            } if unit == &helper_owner.unit && module == &helper_owner.module => {
                Some((version.clone(), *original_ordinal, binder.clone()))
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert!(
        !original_root_imports.is_empty(),
        "the actual ordinary root has a nonempty HOME source import"
    );
    assert!(original_root_imports
        .iter()
        .all(|(version, _, _)| version == &original[1].owner().module_version));

    let directory = Arc::new(crate::artifacts::test_support::compiler_input_directory().unwrap());
    let bodies = ["SourceOriginalSupport.hs", "PublicationHelper.hs"]
        .into_iter()
        .map(|name| {
            (
                PathBuf::from(name),
                Arc::<[u8]>::from(std::fs::read(fixture.includes[0].join(name)).unwrap()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut roots = vec![CapturedCompilerSourceRoot {
        manifest: crate::cache::SourceRootManifest::from_file_digests(
            bodies
                .iter()
                .map(|(path, body)| (path.clone(), blake3::hash(body).to_hex().to_string())),
        )
        .unwrap(),
        bodies,
    }];
    let prelude = PathBuf::from(
        std::env::var_os("TIDEPOOL_PRELUDE_DIR").expect("declared World compiler source roots"),
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
                    .expect("the declared source roots fit the capture owner allowance");
                bodies.insert(
                    path.strip_prefix(&prelude).unwrap().to_owned(),
                    Arc::<[u8]>::from(body),
                );
            }
        }
    }
    roots.push(CapturedCompilerSourceRoot {
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
            roots,
            Arc::from(&b"nonempty-retained-core-promotion"[..]),
            directory,
        )
        .unwrap(),
    );
    let source =
        WorldSourceSelection::new(*sources.seal(), "nonempty-retained-core-promotion").unwrap();
    let mut primary = None;
    let mut previous_origin = None;
    for phase in 0..2 {
        let settled = settle_compiler_transaction_cancellable(
            CompilerTransactionCancellation::new(),
            |_| {},
            || {
                let configured =
                    crate::toolchain::CompilerDeploymentConfiguration::from_env().unwrap();
                let (endpoint, _, _) =
                    crate::toolchain::bind_admitted_extract_endpoint(&configured).unwrap();
                let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint)
                    .map_err(|error| crate::CompileError::ExtractFailed(error.to_string()))?;
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
                        source: if phase == 0 { "import SourceOriginalSupport ()" }
                            else { "import qualified SourceOriginalSupport\nlet answer = SourceOriginalSupport.second" }.into(),
                        template: include_str!("../../../tests/fixtures/world-native/CellTemplate.hs").into(),
                        turn_templates: Vec::new(),
                    }), None,
                )?;
                assert_eq!(plan.items().len(), if phase == 0 { 1 } else { 2 });
                let origin = CompilerTransactionOrigin::current()
                    .expect("the actual World source transaction");
                if let Some(previous) = &previous_origin {
                    assert_ne!(previous, &origin);
                }
                let subject = WorldCellAdmissionSubject::for_source_transaction(
                    &origin,
                    [61; 16],
                    phase,
                    1,
                    1,
                    [b"actual source grant for retained-Core import history".to_vec()],
                )?;
                let mut requests = vec![WorldCellReservationRequest::Prologue {
                    declaration: 190 + phase * 2,
                }];
                if phase == 1 {
                    requests.push(WorldCellReservationRequest::Bind {
                        value: 193,
                        bindings: vec![(
                            "answer".into(),
                            tidepool_repr::SessionVarId::from_extract(
                                tidepool_codegen::prepared_program::session_var_id(
                                    &tidepool_repr::SessionModule::val(tidepool_repr::Generation(
                                        193,
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
                    crate::CompileError::ExtractFailed(format!(
                        "actual retained-Core history refused: {refusal:?}"
                    ))
                })?;
                let mut emitted = BTreeMap::new();
                let mut packet = None;
                let mut raw_imports = None;
                let prepared =
                    crate::artifacts::prepare_world_cell_program_observing_home_contracts(
                        Arc::clone(&checked),
                        &mut |_, originals| {
                            let stage = originals.captured_stage();
                            if let Some(root) = originals.receipt().modules.iter().find(|module| {
                                module.unit == root_owner.unit
                                    && module.module == root_owner.module
                                    && module.origin == ProductOrigin::RetainedCore
                            }) {
                                let imports = root.groups.iter().flat_map(|group| &group.globals)
                                    .filter_map(|global| match &global.owner {
                                        ReceiptImportOwner::Source { unit, module, module_version, original_ordinal, binder }
                                            if unit == &helper_owner.unit && module == &helper_owner.module => {
                                                assert!(module_version.is_none(), "the actual worker leaves promoted Source versions for the certifier");
                                                Some((*original_ordinal, binder.clone()))
                                            }
                                        _ => None,
                                    }).collect::<BTreeSet<_>>();
                                assert!(!imports.is_empty(), "the raw actual root packet has a nonempty Source import before item selection");
                                if let Some(previous) = &raw_imports {
                                    assert_eq!(previous, &imports);
                                }
                                raw_imports = Some(imports);
                            }
                            for entry in stage.producer_certificates().iter().filter(|entry| {
                                entry.descriptor.owner == root_owner
                                    || entry.descriptor.owner == helper_owner
                            }) {
                                if matches!(entry.payload, ArtifactPayload::Original(_)) {
                                    if let Some((previous, _)) = emitted.insert(
                                        entry.descriptor.owner.clone(),
                                        (Arc::clone(entry), Arc::clone(stage)),
                                    ) {
                                        assert!(Arc::ptr_eq(&previous, entry), "later item projections retain one actual physical acquisition");
                                    }
                                }
                            }
                            if originals.receipt().modules.iter().any(|module| {
                                module.unit == helper_owner.unit
                                    && module.module == helper_owner.module
                                    && module.origin == ProductOrigin::RetainedCore
                            }) {
                                packet = Some((
                                    originals.raw().copy_for_publication().unwrap(),
                                    originals.receipt().clone(),
                                ));
                            }
                        },
                    )?;
                Ok::<_, crate::CompileError>((
                    prepared,
                    checked,
                    emitted,
                    packet,
                    raw_imports,
                    origin,
                ))
            },
        );
        assert!(matches!(settled.close(), CompilerTransactionClose::Clean));
        let cancellation = settled
            .host_admission_cancellation()
            .expect("actual post-END owner")
            .clone();
        let (prepared, _checked, emitted, packet, raw_imports, origin) =
            settled.into_outcome().action.unwrap();
        let program = prepared
            .seal(&cancellation)
            .expect("seal the actual World producer");
        let continuation = program.items().last().unwrap().compiler_context();
        if phase == 0 {
            assert!(continuation.source_scope().admits(&root_owner));
            assert!(continuation.compiler_roles().get(&root_owner).is_some());
            assert!(continuation
                .compiler_roles()
                .original_acquisition(&root_owner)
                .is_none());
            assert!(
                continuation.compiler_roles().get(&helper_owner).is_some(),
                "the import-only phase supplies the helper's canonical TYPE row"
            );
            assert!(
                continuation
                    .compiler_roles()
                    .original_acquisition(&helper_owner)
                    .is_none(),
                "the later helper production requires an import-only input without its native Original"
            );
            let support = continuation.compiler_source_support();
            assert!(
                support
                    .native_offers()
                    .iter()
                    .all(|offer| { offer.original_acquisition().descriptor.owner != helper_owner }),
                "the import-only phase cannot carry an existing helper native offer"
            );
            assert!(
                support.native_availability().rows().all(|row| {
                    row.original_owner().unit != helper_owner.unit
                        || row.original_owner().module != helper_owner.module
                }),
                "the later helper re-emission requires no carried helper native availability"
            );
            assert!(
                emitted.is_empty(),
                "the import-only phase cannot already produce either native body"
            );
            primary = Some(Arc::clone(continuation));
            previous_origin = Some(origin);
            continue;
        }
        let actual = [&root_owner, &helper_owner].map(|owner| {
            let (entry, stage) = emitted
                .get(owner)
                .expect("the actual retained-Core cohort emits root and helper");
            let ArtifactPayload::Original(product) = &entry.payload else {
                unreachable!()
            };
            assert!(product.source_sha256().is_none());
            assert!(product.source_admission().is_none());
            assert!(product.execution_source().is_none());
            let offer = stage
                .source_selection()
                .completed_compiler_output_offer(owner)
                .unwrap()
                .unwrap();
            assert!(Arc::ptr_eq(offer.original_acquisition(), entry));
            (entry, stage, product)
        });
        for index in 0..2 {
            let (entry, _, product) = actual[index];
            assert_eq!(
                product.module_interface(),
                original[index].module_interface()
            );
            assert_eq!(product.product_bytes(), original[index].product_bytes());
            assert_ne!(
                product.owner().module_version,
                original[index].owner().module_version
            );
            assert_ne!(
                entry.descriptor.id,
                [&fixture.root, &fixture.helper][index].descriptor.id
            );
        }
        let (_, root_stage, root) = actual[0];
        let (helper_entry, _, helper) = actual[1];
        let root_imports = root
            .original_native()
            .groups
            .iter()
            .flat_map(|group| group.imports().iter())
            .filter_map(|import| match import {
                ReceiptImportOwner::Source {
                    unit,
                    module,
                    module_version: Some(version),
                    original_ordinal,
                    binder,
                } if unit == &helper_owner.unit && module == &helper_owner.module => {
                    Some((version.clone(), *original_ordinal, binder.clone()))
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert!(!root_imports.is_empty());
        assert!(root_imports
            .iter()
            .all(|(version, _, _)| version == &helper.owner().module_version));
        assert_ne!(
            root_imports, original_root_imports,
            "source imports rekey to the actual promoted helper owner"
        );
        assert_eq!(
            root_imports
                .iter()
                .map(|(_, ordinal, binder)| (*ordinal, binder.clone()))
                .collect::<BTreeSet<_>>(),
            original_root_imports
                .iter()
                .map(|(_, ordinal, binder)| (*ordinal, binder.clone()))
                .collect()
        );
        assert_eq!(
            root_imports
                .iter()
                .map(|(_, ordinal, binder)| (*ordinal, binder.clone()))
                .collect::<BTreeSet<_>>(),
            raw_imports.expect("the real worker emitted unversioned Source imports"),
        );
        let root_offer = root_stage
            .source_selection()
            .completed_compiler_output_offer(&root_owner)
            .unwrap()
            .unwrap();
        let graph = root_offer
            .proof()
            .expect("the completed output carries its own exact native graph")
            .selection
            .graph_selection();
        let root_id = actual[0].0.descriptor.id;
        let helper_id = helper_entry.descriptor.id;
        let helper_offer = actual[1]
            .1
            .source_selection()
            .completed_compiler_output_offer(&helper_owner)
            .unwrap()
            .unwrap();
        let helper_groups = helper_offer
            .proof()
            .expect("the actual helper producer retains its own scoped group proof")
            .selection
            .graph_selection()
            .native_groups
            .iter()
            .filter(|group| group.binding.artifact() == helper_id)
            .copied()
            .collect::<BTreeSet<_>>();
        assert!(!helper_groups.is_empty());
        let expected_edges = root
            .original_native()
            .groups
            .iter()
            .flat_map(|group| {
                group.imports().iter().filter_map(|import| match import {
                    ReceiptImportOwner::Source {
                        unit,
                        module,
                        module_version: Some(version),
                        original_ordinal,
                        ..
                    } if unit == &helper.owner().unit
                        && module == &helper.owner().module
                        && version == &helper.owner().module_version =>
                    {
                        Some((group.group().original_ordinal(), *original_ordinal))
                    }
                    _ => None,
                })
            })
            .collect::<BTreeSet<_>>();
        assert!(!expected_edges.is_empty());
        let edges = graph
            .selections
            .iter()
            .flat_map(|scope| &scope.dependencies)
            .filter_map(|(from, to, edge)| match (from, to, edge) {
                (
                    ArtifactBindingNode::Group(parent),
                    ArtifactBindingNode::Group(child),
                    ArtifactDependency::NativeGroup {
                        dependent_ordinal,
                        required_ordinal,
                    },
                ) if parent.binding.artifact() == root_id
                    && child.binding.artifact() == helper_id =>
                {
                    assert_eq!(*dependent_ordinal, parent.original_ordinal);
                    assert_eq!(*required_ordinal, child.original_ordinal);
                    assert!(
                        helper_groups.contains(child),
                        "the root dependency retains the actual helper producer's scoped group"
                    );
                    Some((*dependent_ordinal, *required_ordinal))
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            edges, expected_edges,
            "the exact graph preserves every parent-to-helper import and introduces none"
        );
        assert!(!graph.native_groups.iter().any(|group|
            group.binding.artifact() == fixture.root.descriptor.id || group.binding.artifact() == fixture.helper.descriptor.id),
            "the prior ordinary products are independent evidence, not selected promoted group owners");
        assert_actual_selected_and_retained_history(
            &fixture,
            root_stage,
            [actual[0].0, actual[1].0],
        );
        let (raw, packet) =
            packet.expect("the actual producer recorded retained-Core helper receipt bytes");
        assert_actual_selected_native_refusal(&fixture, raw, packet);
    }
}

fn assert_actual_selected_and_retained_history(
    fixture: &crate::artifacts::test_support::SourcePublicationEntryFixture,
    promoted_stage: &crate::declaration_context::CapturedCompilerStage,
    promoted: [&Arc<crate::artifact_inventory::ArtifactEntry>; 2],
) {
    use crate::artifact_inventory::{ArtifactBindingNode, ArtifactDependency, ArtifactPayload};

    let ordinary = fixture
        .entry
        .products()
        .original_execution_context()
        .unwrap();
    let owners = BTreeSet::from([
        fixture.root.descriptor.owner.clone(),
        fixture.helper.descriptor.owner.clone(),
    ]);
    let completed = promoted_stage.compiler_inputs().compiler_source_support();
    let promoted_support = completed
        .promote_issued_authored_originals(completed, &owners)
        .expect("the actual completed cohort qualifies its own carried Original offers")
        .source_support(&owners)
        .unwrap()
        .finalize_namespace()
        .unwrap();
    let ordinary_completed = ordinary.compiler_source_support();
    let ordinary_support = ordinary_completed
        .promote_issued_authored_originals(ordinary_completed, &owners)
        .expect("ordinary source products select only their actual carried Original offers")
        .source_support(&owners)
        .unwrap()
        .finalize_namespace()
        .unwrap();
    let supports = [ordinary_support, promoted_support];
    let acquisitions = [[&fixture.root, &fixture.helper], promoted];
    let native_edges = |support: &crate::artifact_inventory::CompilerSourceSupport,
                        root: &Arc<crate::artifact_inventory::ArtifactEntry>,
                        helper| {
        support
                .native_availability()
                .issued_row(root.descriptor.id)
                .expect("selected Original has actual issued native availability")
                .offers()
                .filter(|offer| Arc::ptr_eq(offer.original_acquisition(), root))
                .flat_map(|offer| offer.custody_snapshot().binding_dependencies().unwrap())
                .filter(|(from, to, edge)| {
                    matches!((from, to, edge),
                (ArtifactBindingNode::Group(parent), ArtifactBindingNode::Group(child),
                    ArtifactDependency::NativeGroup { .. })
                    if parent.binding.artifact() == root.descriptor.id && child.binding.artifact() == helper)
                })
                .collect::<BTreeSet<_>>()
    };
    let expected_edges = [0, 1].map(|index| {
        let edges = native_edges(
            &supports[index],
            acquisitions[index][0],
            acquisitions[index][1].descriptor.id,
        );
        assert!(
            !edges.is_empty(),
            "each real acquisition's offer retains its nonempty source dependency: {index}"
        );
        edges
    });
    let expected_owners = acquisitions.map(|entries| {
        entries.map(|entry| {
            let ArtifactPayload::Original(product) = &entry.payload else {
                unreachable!()
            };
            product.owner().clone()
        })
    });
    let mut custody = supports[0].artifacts().clone();
    let mut retained = BTreeSet::new();
    // This selects already issued acquisitions. The compiler is not invoked
    // again, and ordinary and retained-Core versions have different domains.
    for index in [0, 1, 0] {
        custody = custody.retain_custody(supports[index].artifacts()).unwrap();
        retained.extend(expected_owners[index].iter().cloned());
        let selected = supports[index].retain_custody(&custody).unwrap();
        let context = ExactDeclarationContext::empty()
            .extend_issued_compiler_support(&selected)
            .unwrap();
        let retained_products = context.recovery_products();
        for entry in acquisitions[index] {
            let acquired = context
                .compiler_source_support()
                .compiler_roles()
                .original_acquisition(&entry.descriptor.owner)
                .expect("the selected real Original remains a compiler role");
            assert!(
                Arc::ptr_eq(acquired, entry),
                "custody growth cannot choose another actual acquisition"
            );
            let availability = context
                .compiler_source_support()
                .native_availability()
                .issued_row(entry.descriptor.id)
                .expect("the selected original retains its issued native availability");
            assert!(availability
                .offers()
                .any(|offer| Arc::ptr_eq(offer.original_acquisition(), entry)));
            let issued = supports[index]
                .native_availability()
                .issued_row(entry.descriptor.id)
                .unwrap();
            for expected in issued.offers() {
                assert!(
                    availability
                        .offers()
                        .any(|offer| offer.same_issuance(expected)),
                    "selection carries the actual offer rather than rebuilding its graph"
                );
            }
        }
        assert_eq!(
            context
                .compiler_original_products()
                .unwrap()
                .iter()
                .filter(
                    |product| owners.iter().any(|owner| owner.unit == product.owner().unit
                        && owner.module == product.owner().module)
                )
                .map(|product| product.owner().clone())
                .collect::<BTreeSet<_>>(),
            expected_owners[index].iter().cloned().collect(),
        );
        assert_eq!(
            retained_products
                .iter()
                .filter(
                    |product| owners.iter().any(|owner| owner.unit == product.owner().unit
                        && owner.module == product.owner().module)
                )
                .map(|product| product.owner().clone())
                .collect::<BTreeSet<_>>(),
            retained,
        );
        assert_eq!(
            native_edges(
                context.compiler_source_support(),
                acquisitions[index][0],
                acquisitions[index][1].descriptor.id
            ),
            expected_edges[index],
            "the selected offer preserves its exact issuing helper graph",
        );
        for history in 0..2 {
            if expected_owners[history]
                .iter()
                .all(|owner| retained.contains(owner))
            {
                // Historical custody does not add executable Group roots to
                // the current view. It retains each exact native witness.
                for entry in acquisitions[history] {
                    let ArtifactPayload::Original(expected) = &entry.payload else {
                        unreachable!()
                    };
                    let retained = retained_products
                        .iter()
                        .find(|product| product.owner() == expected.owner())
                        .expect("historical Original remains in certified custody");
                    assert!(
                        Arc::ptr_eq(retained.original_native(), expected.original_native()),
                        "custody must retain the original dependency witness unchanged"
                    );
                }
            }
        }
    }
    assert_eq!(retained.len(), 4);
}

fn assert_actual_selected_native_refusal(
    fixture: &crate::artifacts::test_support::SourcePublicationEntryFixture,
    raw: ParsedModuleProducts,
    mut packet: CertifiedReceipt,
) {
    let context = fixture
        .entry
        .products()
        .original_execution_context()
        .unwrap();
    let exact = crate::artifacts::test_support::ExactSourceFixture::inspect(
        Some(Arc::new(
            crate::declaration_context::ExactCompileContext::new(context),
        )),
        "module Consumer where\nresult :: Int\nresult = 17\n__tidepool_inspect_0 = result\n",
        "result",
    )
    .expect("an actual exact request completes while retaining the selected native Originals");
    // A retained-Core module row is adversarial input here. It cannot replace
    // this request's selected native owner, even though its bytes came from a
    // genuine successful producer under a different TYPE-only input selection.
    packet.modules.retain(|module| {
        module.unit == fixture.helper.descriptor.owner.unit
            && module.module == fixture.helper.descriptor.owner.module
    });
    assert_eq!(packet.modules.len(), 1);
    assert_eq!(packet.modules[0].origin, ProductOrigin::RetainedCore);
    packet.targets.clear();
    let request = exact.request();
    let before = request
        .compiler_inputs()
        .artifacts()
        .capture_graph_selection();
    let mut evidence: DependencyEvidence =
        serde_json::from_slice(exact.admissions()[0].evidence_bytes()).unwrap();
    evidence.cache_safe = false;
    evidence.selection_complete = false;
    let bytes = serde_json::to_vec(&evidence).unwrap();
    let source = exact.admissions()[0]
        .match_product(
            request,
            exact.source_path(),
            exact.source(),
            &bytes,
            raw.operation(),
        )
        .unwrap();
    let issues = HOME_CERTIFICATION_ISSUES.with(std::cell::Cell::get);
    let error = certify_products(
        None,
        &packet,
        &raw,
        &crate::artifacts::SourceEvidenceAdmission::from_exact(source),
        exact.receipt_directory(),
        &fixture.producer_bytes,
        &fixture.includes,
        None,
    )
    .err()
    .expect("retained Core cannot replace a selected native owner");
    let crate::artifact_inventory::ArtifactPayload::Original(helper) = &fixture.helper.payload
    else {
        unreachable!()
    };
    assert!(
        matches!(error, CertificationError::RetainedCoreReplacesNativeOwner { owner } if owner == *helper.owner())
    );
    assert_eq!(HOME_CERTIFICATION_ISSUES.with(std::cell::Cell::get), issues);
    assert_eq!(
        request
            .compiler_inputs()
            .artifacts()
            .capture_graph_selection(),
        before
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config { cases: 8, ..Default::default() })]
    #[test]
    fn segment_item_count_does_not_repeat_original_proof_issuance(item_count in 1_usize..25) {
        check_retained_core_promotion_history(item_count);
    }
}

fn check_retained_core_promotion_history(item_count: usize) {
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
    let parsed = ParsedModuleProducts::decode(&initial_bytes, &package_bundle()).unwrap();
    assert_eq!(parsed.products().len(), 2);
    let accepted = ordinary_receipts(&initial_bytes, &parsed, &initial_worker);
    let packet = CertifiedReceipt {
        package_products: Vec::new(),
        package_links: PackageLinks::empty(),
        source_recipe: WorkerExecutionSource::Ordinary,
        finalization: fixture_finalization_from_products(Some(root.path()), &accepted, &parsed),
        modules: accepted,
        targets: BTreeMap::new(),
        packages: BTreeMap::new(),
    };
    let initial = certify_products(
        None,
        &packet,
        &parsed,
        &crate::artifacts::admit_compiler_source(
            (&parsed).operation(),
            &initial_evidence_bytes,
            &initial_input,
            target_source,
        )
        .unwrap(),
        root.path(),
        &producer,
        &include,
        None,
    )
    .unwrap();
    let prior = initial.data().recovery_products.clone();
    assert_eq!(prior.len(), 2);
    let initial_a = prior.iter().find(|p| p.owner().module == A).unwrap();
    let initial_b = prior.iter().find(|p| p.owner().module == B).unwrap();
    let canonical = prior
        .iter()
        .map(|product| {
            (
                product.owner().module.clone(),
                product.module_interface().clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(initial_a.original_native().groups.len(), 2);
    assert_eq!(initial_b.original_native().groups.len(), 2);
    assert!(initial_a.original_native().groups.iter().all(|g| {
        matches!(&g.imports[..], [ReceiptImportOwner::Source { unit, module, module_version, original_ordinal, binder: imported }] if unit == &initial_b.owner().unit && module == &initial_b.owner().module && module_version.as_ref() == Some(&initial_b.owner().module_version) && *original_ordinal == B_ORDINAL && imported == &binder(B, B_ORDINAL))
    }));
    let initial = crate::declaration_context::issue_certified_product_native_availability(
        canonical_producer,
        initial,
        None,
        &mut PackageInterfaceValidation::with_inventory(Arc::clone(parsed.operation())),
    )
    .unwrap();
    let initial_output = initial.captured_stage().output();
    assert!(initial
        .compiler_inputs()
        .issued_artifacts()
        .selected_native_groups()
        .is_empty());
    for (product, ordinals) in [
        (initial_a, BTreeSet::from([A_ORDINAL, A_ORDINAL + 1])),
        (initial_b, BTreeSet::from([B_ORDINAL, B_ORDINAL + 1])),
    ] {
        let owner = crate::declaration_join::ExactModuleIdentity {
            unit: product.owner().unit.clone(),
            module: product.owner().module.clone(),
        };
        let receipt = initial
            .source_selection()
            .produced_source_receipt(&owner, initial.compiler_inputs().issued_artifacts())
            .unwrap()
            .unwrap();
        let artifact = crate::artifact_inventory::ArtifactEntry::original_artifact_id(
            canonical_producer,
            product,
        );
        let actual = receipt
            .row()
            .offers()
            .flat_map(|offer| offer.proof())
            .flat_map(|proof| proof.selection.graph_selection().native_groups.iter())
            .filter(|group| group.binding.artifact() == artifact)
            .map(|group| group.original_ordinal)
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, ordinals);
        for original_ordinal in ordinals {
            assert_eq!(
                receipt
                    .groups(crate::artifact_inventory::NativeGroupKey {
                        artifact,
                        original_ordinal,
                    })
                    .count(),
                1
            );
        }
    }
    let initial_support = crate::artifact_inventory::CompilerSourceSupport::close_complete_output(
        initial_output.compiler_inputs(),
    )
    .unwrap();
    let mut context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_issued_compiler_support(&initial_support)
        .unwrap();

    let selected_native_context = Arc::new(context.clone());

    // The exact request exposes canonical interfaces while withholding native
    // originals from the compiler input.
    let projection = context.compiler_input_projection().interface_only();
    context = context.recover_fixture_projection(projection).unwrap();
    assert!(context.artifact_view().selected_native_groups().is_empty());
    let directory = root.path().join("promotion-0");
    std::fs::create_dir(&directory).unwrap();
    let input = directory.join("Target.hs");
    std::fs::write(&input, target_source).unwrap();
    let request = Arc::new(context.clone())
        .prepare_compilation(&directory.join("inputs"), &producer)
        .unwrap();
    assert!(request.compiler_original_products().unwrap().is_empty());
    // Current source owns Target only; A and B are emitted from authenticated
    // retained Core rather than observed as fresh source modules.
    let mut current_evidence = evidence(target_source);
    current_evidence.modules[0].module = "Target".into();
    let mut worker = current_evidence.clone();
    worker.sources.last_mut().unwrap().path = input.clone();
    let complete_evidence = serde_json::to_vec(&worker).unwrap();
    let receipt_root = directory.join(".exact-compilations/current");
    std::fs::create_dir_all(&receipt_root).unwrap();
    let snapshot = receipt_root.join("source.hs");
    std::fs::write(&snapshot, target_source).unwrap();
    let exact_receipt = value_array([
        value_text("TPEXACTCOMPILE"),
        value_text("4"),
        value_text(&request.request_sha256),
        value_text(hex(&request.semantic_sha256)),
        value_text(input.to_string_lossy()),
        value_text(hex(&sha(target_source.as_bytes()))),
        value_text(snapshot.to_string_lossy()),
        value_text(String::from_utf8(complete_evidence.clone()).unwrap()),
        value_array([value_array([
            value_text(UNIT),
            value_text("Target"),
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
        .admit_source(&input, target_source, &evidence_bytes)
        .unwrap();
    let exact = (&request).product_admission(&source_admission);

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
            let canonical = original.module_interface();
            let mut accepted = receipt(&promoted_bytes, &worker, "");
            accepted.origin = ProductOrigin::RetainedCore;
            accepted.unit = UNIT.into();
            accepted.module = product.module.clone();
            accepted.skinny_iface_sha256 = sha(&product.interface);
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
        package_products: Vec::new(),
        package_links: PackageLinks::empty(),
        source_recipe: WorkerExecutionSource::ExactUnavailable(
            SourceRecipeUnavailable::NoFreshOriginals,
        ),
        finalization: fixture_finalization_from_products(None, &promoted_receipts, &promoted),
        modules: promoted_receipts,
        targets: BTreeMap::new(),
        packages: BTreeMap::new(),
    };
    {
        let denied_directory = root.path().join("selected-native-refusal");
        std::fs::create_dir(&denied_directory).unwrap();
        let denied_request = selected_native_context
            .prepare_compilation(&denied_directory.join("inputs"), &producer)
            .unwrap();
        let before_graph = denied_request
            .compiler_inputs()
            .artifacts()
            .capture_graph_selection();
        let before_issues = HOME_CERTIFICATION_ISSUES.with(std::cell::Cell::get);
        let (denied_source, denied_text) =
            crate::declaration_context::generated_source_admission_fixture(
                &denied_directory,
                &denied_request,
            );
        let mut denied_evidence: crate::cache::DependencyEvidence =
            serde_json::from_slice(denied_source.evidence_bytes()).unwrap();
        denied_evidence.cache_safe = false;
        denied_evidence.selection_complete = false;
        let denied_evidence = serde_json::to_vec(&denied_evidence).unwrap();
        let denied_exact = denied_request.product_admission(&denied_source);
        let error = certify_products(
            None,
            &promoted_packet,
            &promoted,
            &crate::artifacts::SourceEvidenceAdmission::from_exact(
                denied_exact
                    .source()
                    .match_product(
                        denied_exact.physical_request().unwrap(),
                        &denied_directory.join("Consumer.hs"),
                        &denied_text,
                        &denied_evidence,
                        promoted.operation(),
                    )
                    .unwrap(),
            ),
            root.path(),
            &producer,
            &include,
            None,
        )
        .err()
        .expect("retained Core cannot replace an exact selected native owner");
        assert!(
            matches!(&error, CertificationError::RetainedCoreReplacesNativeOwner { owner }
            if owner == initial_a.owner())
        );
        assert_eq!(
            HOME_CERTIFICATION_ISSUES.with(std::cell::Cell::get),
            before_issues
        );
        assert_eq!(
            denied_request
                .compiler_inputs()
                .artifacts()
                .capture_graph_selection(),
            before_graph
        );
        let error = crate::CompileError::CompilerEvidence(Box::new(error));
        assert_eq!(error.to_string(), "compiler evidence rejected: compiler product certificate disagrees with retained core replaces native owner");
        let envelope = crate::classify_compile(&error);
        assert_eq!(envelope.class, crate::FailureClass::VersionSkew);
        assert_eq!(envelope.phase, crate::Phase::Compile);
        assert_eq!(
            envelope.cause,
            Some(crate::failclass::CompileFailureCause::CompilerEvidence {
                failure: crate::failclass::CompilerEvidenceFailure::Contract,
            })
        );
    }
    drop(selected_native_context);

    let mut failed = promoted_packet.clone();
    let duplicate_group = failed.modules[0].groups[0].clone();
    failed.modules[0].groups.push(duplicate_group);
    assert!(certify_products(
        None,
        &failed,
        &promoted,
        &crate::artifacts::SourceEvidenceAdmission::from_exact(
            (&exact)
                .source()
                .match_product(
                    (&exact).physical_request().unwrap(),
                    &input,
                    target_source,
                    &evidence_bytes,
                    (&promoted).operation()
                )
                .unwrap()
        ),
        root.path(),
        &producer,
        &include,
        None,
    )
    .is_err());
    let certified = certify_products(
        None,
        &promoted_packet,
        &promoted,
        &crate::artifacts::SourceEvidenceAdmission::from_exact(
            (&exact)
                .source()
                .match_product(
                    (&exact).physical_request().unwrap(),
                    &input,
                    target_source,
                    &evidence_bytes,
                    (&promoted).operation(),
                )
                .unwrap(),
        ),
        root.path(),
        &producer,
        &include,
        None,
    )
    .unwrap();
    let certified = crate::declaration_context::issue_certified_product_native_availability(
        canonical_producer,
        certified,
        Some(request.compiler_inputs().artifacts()),
        &mut PackageInterfaceValidation::with_inventory(Arc::clone(promoted.operation())),
    )
    .unwrap();
    let output = certified.captured_stage().output();
    let selected_owners = certified
        .source_selection()
        .receipt_native_body_owners()
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
        .data()
        .recovery_products
        .iter()
        .find(|p| selected_owners.contains(p.owner()) && p.owner().module == A)
        .unwrap();
    let current_b = certified
        .data()
        .recovery_products
        .iter()
        .find(|p| selected_owners.contains(p.owner()) && p.owner().module == B)
        .unwrap();
    assert_eq!(current_a.original_native().groups.len(), 2);
    assert_eq!(
        current_a
            .original_native()
            .groups
            .iter()
            .map(|group| group.group().original_ordinal())
            .collect::<Vec<_>>(),
        [7, 8]
    );
    assert_eq!(
        current_b
            .original_native()
            .groups
            .iter()
            .map(|group| group.group().original_ordinal())
            .collect::<Vec<_>>(),
        [11, 12]
    );
    let expected_versions = expected_promoted_versions(&promoted, &canonical);
    assert_eq!(current_a.owner().module_version, expected_versions[A]);
    assert_eq!(current_b.owner().module_version, expected_versions[B]);
    assert!(current_a.original_native().groups.iter().all(|g| {
        matches!(&g.imports[..], [ReceiptImportOwner::Source { unit, module, module_version, original_ordinal, binder: imported }] if unit == &current_b.owner().unit && module == &current_b.owner().module && module_version.as_ref() == Some(&current_b.owner().module_version) && *original_ordinal == B_ORDINAL && imported == &binder(B, B_ORDINAL))
    }));

    let inventory_decodes_before = MODULE_PRODUCT_INVENTORY_DECODES.with(std::cell::Cell::get);
    let receipt_decodes_before = SEGMENT_ORIGINAL_RECEIPT_DECODES.with(std::cell::Cell::get);
    let item_decodes_before = SEGMENT_ITEM_RECEIPT_DECODES.with(std::cell::Cell::get);
    let shared_raw = ParsedModuleProducts::decode(&promoted_bytes, &package_bundle()).unwrap();
    let mut validation = PackageInterfaceValidation::with_inventory(shared_raw.operation().clone());
    // Encode current neutral facts, then enter the strict production decoder.
    let modules = promoted_packet.modules.iter().map(|module| {
        value_array([
            value_text(match module.origin {
                ProductOrigin::Fresh => "fresh",
                ProductOrigin::Cached => "cached",
                ProductOrigin::RetainedCore => "retained-core",
            }),
            value_text(&module.unit),
            value_text(&module.module),
            module
                .module_version
                .as_ref()
                .map_or(Value::Null, |version| value_text(hex(&version.0))),
            value_text(hex(&module.source_sha256)),
            value_text(hex(&module.skinny_iface_sha256)),
            value_text(hex(&module.product_sha256)),
            value_text(hex(&module.dependency_witness_sha256)),
            value_array(module.groups.iter().map(|group| {
                value_array([
                    Value::Integer((group.original_ordinal as u64).into()),
                    value_array(group.globals.iter().map(value_global)),
                ])
            })),
        ])
    });
    let mut facts = empty_product_facts();
    facts.as_array_mut().unwrap()[2] = value_array(modules);
    facts.as_array_mut().unwrap()[3] =
        value_array([value_array([value_text("item"), value_array([])])]);
    let mut wire = dictionary_receipt(&facts);
    let payload = receipt_payload_mut(&mut wire);
    payload[0] = value_text("segment-originals");
    payload.remove(2);
    payload[4] = fixture_envelope_value(&promoted_packet.finalization);
    assert!(matches!(
        promoted_packet.source_recipe,
        WorkerExecutionSource::ExactUnavailable(SourceRecipeUnavailable::NoFreshOriginals)
    ));
    payload[5] = value_array([
        value_text("exact-unavailable"),
        value_text("no-fresh-originals"),
    ]);
    for (row, module) in payload[1]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(&promoted_packet.modules)
    {
        row.as_array_mut().unwrap()[9] = value_array(module.interface_requirements.iter().map(
            |((unit, name), seal)| {
                value_array([value_text(unit), value_text(name), value_text(hex(seal))])
            },
        ));
    }
    let shared_receipt = decode_segment_originals_with_operation(
        &receipt_bytes(&wire),
        root.path(),
        &validation.inventory,
    )
    .unwrap();
    let inventory = validation.inventory.clone();
    let bound_source = exact
        .source()
        .match_product(
            exact.physical_request().unwrap(),
            &input,
            target_source,
            &evidence_bytes,
            &inventory,
        )
        .unwrap();
    let package_interfaces =
        capture_independent_package_interfaces(&shared_receipt, &mut validation).unwrap();
    let shared = CertifiedSegmentOriginals::issue(
        shared_raw,
        shared_receipt,
        bound_source,
        root.path(),
        &producer,
        &include,
        None,
        None,
        &crate::artifacts::SelectedSessionValueOwners::capture(std::iter::empty()).unwrap(),
        None,
        &package_interfaces,
        &mut validation,
    )
    .unwrap();
    assert_eq!(
        MODULE_PRODUCT_INVENTORY_DECODES.with(std::cell::Cell::get) - inventory_decodes_before,
        1
    );
    assert_eq!(
        SEGMENT_ORIGINAL_RECEIPT_DECODES.with(std::cell::Cell::get) - receipt_decodes_before,
        1
    );
    let mut cursor = shared.bound_artifacts().unwrap();
    let immutable_counts = (
        MODULE_PRODUCT_INVENTORY_DECODES.with(std::cell::Cell::get),
        ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get),
        HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get),
    );
    let prepared_artifact = crate::prepared_artifact::PreparedArtifact::parse(
        tidepool_test_data::prepared_encode::encode_wire_program(&testing::wire_program()),
        tidepool_repr::execution_schema::DecodeLimits::default(),
    )
    .unwrap();
    let prepared_binding = prepared_artifact.bind_package_program();
    let empty_delta = receipt_bytes(&segment_item_receipt(&SegmentItemReceipt {
        globals: vec![],
        packages: BTreeMap::new(),
        selection: PackageSelection {
            program_sha256: *prepared_binding.sha256(),
            ..empty_item_selection(*shared.receipt.package_links.stage_id())
        },
    }));
    let native_issues = HOME_CERTIFICATION_ISSUES.with(std::cell::Cell::get);
    for item in 0..item_count {
        let (selected, globals, additions, _) = shared
            .select_item(
                &mut cursor,
                decode_segment_item_with_operation(&empty_delta, &validation.inventory).unwrap(),
                &prepared_binding,
                None,
                &mut validation,
            )
            .unwrap();
        assert!(globals.is_empty() && additions.is_empty());
        assert!(
            Arc::ptr_eq(&selected.data().groups, &shared.products.data().groups),
            "item {item} shares module groups"
        );
        assert!(Arc::ptr_eq(
            &selected.data().recovery_products,
            &shared.products.data().recovery_products
        ));
        assert!(Arc::ptr_eq(
            &selected.data().module_interfaces,
            &shared.products.data().module_interfaces
        ));
        assert!(Arc::ptr_eq(
            &selected.source_selection().modules,
            &shared.products.source_selection().modules
        ));
        assert_eq!(
            selected
                .source_selection()
                .receipt_native_body_owners()
                .cloned()
                .collect::<BTreeSet<_>>(),
            selected_owners
        );
        assert_eq!(
            (
                MODULE_PRODUCT_INVENTORY_DECODES.with(std::cell::Cell::get),
                ORIGINAL_PRODUCT_DECODES.with(std::cell::Cell::get),
                HOME_CERTIFICATION_DECODES.with(std::cell::Cell::get),
            ),
            immutable_counts,
            "item {item} cannot decode immutable module facts"
        );
        assert_eq!(
            HOME_CERTIFICATION_ISSUES.with(std::cell::Cell::get),
            native_issues,
            "item selection cannot issue another module proof"
        );
    }
    assert_eq!(
        SEGMENT_ITEM_RECEIPT_DECODES.with(std::cell::Cell::get) - item_decodes_before,
        item_count
    );
    // Distinct real file selections are item additions, never shared state.
    for ordinal in 0..2 {
        let path = root.path().join(format!("target-package-{ordinal}.hi"));
        let payload = [0x40 + ordinal as u8];
        std::fs::write(&path, payload).unwrap();
        let identity = SymbolIdentity {
            unit: "external-package".into(),
            module: format!("External{ordinal}"),
            ..testing::identity("unused", "entry")
        };
        let global = AcceptedGlobal {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            owner: ReceiptImportOwner::Package {
                unit: identity.unit.clone(),
                module: identity.module.clone(),
                binder: identity.clone(),
                interface_digest: sha(&payload),
            },
        };
        let mut wire = testing::wire_program();
        wire.globals.push(GlobalDecl {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        let artifact = crate::prepared_artifact::PreparedArtifact::parse(
            tidepool_test_data::prepared_encode::encode_wire_program(&wire),
            tidepool_repr::execution_schema::DecodeLimits::default(),
        )
        .unwrap();
        let target = Arc::clone(artifact.prepared_shared());
        let target_binding = artifact.bind_package_program();
        let owner = (identity.unit.clone(), identity.module.clone());
        let witness = PackageInterfaceWitness {
            selected_path: path.clone(),
            sha256: sha(&payload),
        };
        let addition = || SegmentItemReceipt {
            globals: vec![global.clone()],
            packages: BTreeMap::from([(owner.clone(), witness.clone())]),
            selection: PackageSelection {
                program_sha256: *target_binding.sha256(),
                ..empty_item_selection(*shared.receipt.package_links.stage_id())
            },
        };
        let (selected, globals, packages, _) = shared
            .select_item(
                &mut cursor,
                addition(),
                &target_binding,
                None,
                &mut validation,
            )
            .unwrap();
        assert_eq!(packages.len(), 1);
        assert!(packages.contains_key(&owner));
        assert!(!shared.package_availability().contains_key(&owner));
        let observed =
            AuthenticatedTargetImports::issue(&target, &globals, &validation.inventory).unwrap();
        let physical = shared.captured_stage();
        let admitted = physical
            .issued_artifacts()
            .inventory()
            .select_native_target(
                selected.physical_cohort(),
                observed,
                None,
                None,
                &[],
                &packages,
                &mut validation,
            )
            .unwrap();
        assert!(matches!(
            admitted.imports(),
            [PendingImportOwner::Package { binder, interface_digest, .. }]
                if binder == &identity && *interface_digest == witness.sha256
        ));
        assert!(admitted.artifacts().selected_native_groups().is_empty());
        let (_, _, next_packages, _) = shared
            .select_item(
                &mut cursor,
                SegmentItemReceipt {
                    globals: vec![],
                    packages: BTreeMap::new(),
                    selection: PackageSelection {
                        program_sha256: *prepared_binding.sha256(),
                        ..empty_item_selection(*shared.receipt.package_links.stage_id())
                    },
                },
                &prepared_binding,
                None,
                &mut validation,
            )
            .unwrap();
        assert!(!next_packages.contains_key(&owner));
        let mut unused = addition();
        unused.globals.clear();
        unused.selection.program_sha256 = *prepared_binding.sha256();
        assert!(matches!(
            shared.select_item(
                &mut cursor,
                unused,
                &prepared_binding,
                None,
                &mut validation
            ),
            Err(CertificationError::Mismatch(
                "item package has no target demand"
            ))
        ));
        std::fs::write(&path, [0xFF]).unwrap();
        let mut recapture = PackageInterfaceValidation::with_inventory(shared.operation().clone());
        assert!(matches!(
            shared.select_item(
                &mut cursor,
                addition(),
                &target_binding,
                None,
                &mut recapture,
            ),
            Err(CertificationError::CapturedModulePayload(
                crate::recovery_artifacts::RecoveryArtifactError::DigestMismatch(_)
            ))
        ));
    }
    let mut other_validation = PackageInterfaceValidation::default();
    assert!(matches!(
        shared.select_item(
            &mut cursor,
            SegmentItemReceipt {
                globals: vec![],
                packages: BTreeMap::new(),
                selection: PackageSelection {
                    program_sha256: *prepared_binding.sha256(),
                    ..empty_item_selection(*shared.receipt.package_links.stage_id())
                },
            },
            &prepared_binding,
            None,
            &mut other_validation,
        ),
        Err(CertificationError::Mismatch(
            "segment inventory accounting owner"
        ))
    ));
    let publication = shared.raw().copy_for_publication().unwrap();
    assert_eq!(publication.products(), shared.raw().products());

    let independent = ParsedModuleProducts::decode(&promoted_bytes, &package_bundle()).unwrap();
    let independent_result = certify_products(
        None,
        &promoted_packet,
        &independent,
        &crate::artifacts::SourceEvidenceAdmission::from_exact(
            (&exact)
                .source()
                .match_product(
                    (&exact).physical_request().unwrap(),
                    &input,
                    target_source,
                    &evidence_bytes,
                    (&independent).operation(),
                )
                .unwrap(),
        ),
        root.path(),
        &producer,
        &include,
        None,
    )
    .unwrap();
    let independent_a = independent_result
        .data()
        .recovery_products
        .iter()
        .find(|product| product.owner() == current_a.owner())
        .unwrap();
    assert_eq!(independent_a, current_a);
    assert!(
        !Arc::ptr_eq(independent_a.original_native(), current_a.original_native()),
        "another admission issues its own proof"
    );

    let originals = [current_a.clone(), current_b.clone()];
    let mut first_entry_validation = PackageInterfaceValidation::default();
    let entry_work_start = first_entry_validation.inventory.work_usage().unwrap().0;
    let original_view =
        crate::declaration_context::certified_product_artifact_view_with_validation(
            canonical_producer,
            &originals,
            &[],
            &[],
            None,
            crate::artifact_inventory::NativeArtifactDemand::AllGroups,
            &mut first_entry_validation,
        )
        .unwrap();
    let original_entry_work =
        first_entry_validation.inventory.work_usage().unwrap().0 - entry_work_start;
    let original_entries = original_view
        .entries()
        .into_iter()
        .filter(|entry| {
            matches!(
                &entry.payload,
                crate::artifact_inventory::ArtifactPayload::Original(_)
            )
        })
        .map(|entry| (entry.descriptor.id, entry))
        .collect::<BTreeMap<_, _>>();
    for item in 1..14 {
        let mut repeated_validation = PackageInterfaceValidation::default();
        let reused = crate::declaration_context::certified_product_artifact_view_with_validation(
            canonical_producer,
            &originals,
            &[],
            &[],
            Some(&original_view),
            crate::artifact_inventory::NativeArtifactDemand::AllGroups,
            &mut repeated_validation,
        )
        .unwrap();
        assert_eq!(
            reused.selected_native_groups(),
            original_view.selected_native_groups()
        );
        for entry in reused.entries().iter().filter(|entry| {
            matches!(
                &entry.payload,
                crate::artifact_inventory::ArtifactPayload::Original(_)
            )
        }) {
            assert!(
                Arc::ptr_eq(entry, &original_entries[&entry.descriptor.id]),
                "item {item} shares exact original entry"
            );
        }
        let repeated_work = repeated_validation.inventory.work_usage().unwrap().0;
        if item == 1 {
            println!("artifact_entry_work first={original_entry_work} repeated={repeated_work}");
        }
        assert!(repeated_work < original_entry_work,
            "item {item} avoids rebuilding native requirement maps: first={original_entry_work} repeated={repeated_work}");
    }
    let mut changed_source = promoted_packet.clone();
    changed_source
        .modules
        .iter_mut()
        .find(|module| module.module == A)
        .unwrap()
        .source_sha256 = [99; 32];
    assert!(
        matches!(
            certify_products(
                None,
                &changed_source,
                &independent,
                &crate::artifacts::SourceEvidenceAdmission::from_exact(
                    exact
                        .source()
                        .match_product(
                            exact.physical_request().unwrap(),
                            &input,
                            target_source,
                            &evidence_bytes,
                            independent.operation(),
                        )
                        .unwrap(),
                ),
                root.path(),
                &producer,
                &include,
                None,
            ),
            Err(CertificationError::Mismatch(_))
        ),
        "a mismatched raw source receipt cannot become a reusable completed Original"
    );
    assert!(
        crate::declaration_context::certified_product_artifact_view_with_validation(
            [99; 32],
            &originals,
            &[],
            &[],
            Some(&original_view),
            crate::artifact_inventory::NativeArtifactDemand::AllGroups,
            &mut PackageInterfaceValidation::default()
        )
        .is_err()
    );
    let mut independent_entry_validation = PackageInterfaceValidation::default();
    let distinct_proof = original_view
        .retained_original_entry_with_validation(
            canonical_producer,
            independent_a,
            &mut independent_entry_validation,
        )
        .unwrap();
    assert!(
        distinct_proof.is_none(),
        "different issuer proof requires ordinary metadata admission"
    );

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
    assert!(module_version.is_none());
    *module_version = Some(current_b.owner().module_version.clone());
    let explicit_version = certify_products(
        None,
        &changed_import,
        &promoted,
        &crate::artifacts::SourceEvidenceAdmission::from_exact(
            (&exact)
                .source()
                .match_product(
                    (&exact).physical_request().unwrap(),
                    &input,
                    target_source,
                    &evidence_bytes,
                    (&promoted).operation(),
                )
                .unwrap(),
        ),
        root.path(),
        &producer,
        &include,
        None,
    )
    .err()
    .expect("a raw retained-Core receipt cannot supply even its derived version");
    assert!(
        matches!(
            &explicit_version,
            CertificationError::Mismatch("retained source version must be derived")
        ),
        "unexpected explicit retained version refusal: {explicit_version:?}",
    );

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
        &crate::artifacts::SourceEvidenceAdmission::from_exact(
            (&exact)
                .source()
                .match_product(
                    (&exact).physical_request().unwrap(),
                    &input,
                    target_source,
                    &evidence_bytes,
                    (&promoted).operation()
                )
                .unwrap()
        ),
        root.path(),
        &producer,
        &include,
        None,
    )
    .is_err());

    let mut changed_source = promoted_packet.clone();
    changed_source.modules[0].source_sha256[0] ^= 1;
    assert!(certify_products(
        None,
        &changed_source,
        &promoted,
        &crate::artifacts::SourceEvidenceAdmission::from_exact(
            (&exact)
                .source()
                .match_product(
                    (&exact).physical_request().unwrap(),
                    &input,
                    target_source,
                    &evidence_bytes,
                    (&promoted).operation()
                )
                .unwrap()
        ),
        root.path(),
        &producer,
        &include,
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
            .data()
            .recovery_products
            .iter()
            .map(|product| product.owner().clone())
            .collect::<BTreeSet<_>>(),
        owner_history
    );
    assert_eq!(
        certified.data().recovery_products.len(),
        owner_history.len()
    );
    let completed = crate::artifact_inventory::CompilerSourceSupport::close_complete_output(
        output.compiler_inputs(),
    )
    .unwrap();
    let selected_entries = [current_a, current_b]
        .into_iter()
        .map(|product| {
            Arc::new(
                crate::artifact_inventory::ArtifactEntry::original(
                    canonical_producer,
                    product.clone(),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let selected_projection =
        crate::artifact_inventory::CompilerInputProjection::from_issued_entries(&selected_entries)
            .unwrap();
    let catalog =
        crate::artifact_inventory::NativeDependencyCatalog::from_source(&completed).unwrap();
    let selected_view = selected_projection
        .admit_interfaces_with_native_offers(
            completed.artifacts(),
            selected_entries.clone(),
            completed.native_offers(),
            &catalog,
        )
        .unwrap();
    let selected_support = crate::declaration_context::recover_fixture_compiler_support(
        selected_projection,
        selected_view,
        completed.native_offers().iter().cloned(),
    )
    .unwrap()
    .retain_normalized_availability(completed.native_availability())
    .unwrap();
    let mut history_context = context
        .extend_issued_compiler_support(&selected_support)
        .unwrap();
    for entry in &selected_entries {
        assert_eq!(
            history_context.compiler_input_projection().snapshot_roles()[&entry.descriptor.owner]
                .original(),
            Some(entry.descriptor.id)
        );
        assert_eq!(
            history_context
                .compiler_source_support()
                .native_availability()
                .snapshot_facts()
                .unwrap()[&entry.descriptor.id]
                .mode,
            crate::declaration_context::NativeTransportMode::Full
        );
    }
    assert!(history_context
        .artifact_view()
        .selected_native_groups()
        .is_empty());
    let mut latest_recovery = certified.data().recovery_products.clone();
    for (index, body_tag) in [0, 1, 0].into_iter().enumerate() {
        let product_bytes = raw_products(body_tag);
        let parsed = ParsedModuleProducts::decode(&product_bytes, &package_bundle()).unwrap();
        let next = certify_promotion(
            &history_context,
            index + 1,
            &product_bytes,
            &parsed,
            &promoted_packet,
            &current_evidence,
            target_source,
            &producer,
            &include,
            root.path(),
        );
        let selected = next
            .source_selection()
            .receipt_native_body_owners()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(selected.len(), 2);
        let current_a = next
            .data()
            .recovery_products
            .iter()
            .find(|p| selected.contains(p.owner()) && p.owner().module == A)
            .unwrap();
        let current_b = next
            .data()
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
                .groups
                .iter()
                .map(|group| group.group().original_ordinal())
                .collect::<Vec<_>>(),
            [7, 8]
        );
        assert_eq!(
            current_b
                .original_native()
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
        assert!(current_a.original_native().groups.iter().all(|g| {
            matches!(&g.imports[..], [ReceiptImportOwner::Source { unit, module, module_version, original_ordinal, binder: imported }] if unit == &current_b.owner().unit && module == &current_b.owner().module && module_version.as_ref() == Some(&current_b.owner().module_version) && *original_ordinal == B_ORDINAL && imported == &binder(B, B_ORDINAL))
        }));
        if let Some(previous) = historical_a_imports.get(current_a.owner()) {
            assert_eq!(previous, &native_import_facts(current_a));
        } else {
            historical_a_imports.insert(current_a.owner().clone(), native_import_facts(current_a));
        }
        owner_history.insert(current_a.owner().clone());
        owner_history.insert(current_b.owner().clone());
        assert_eq!(
            next.data()
                .recovery_products
                .iter()
                .map(|product| product.owner().clone())
                .collect::<BTreeSet<_>>(),
            owner_history
        );
        assert_eq!(next.data().recovery_products.len(), owner_history.len());
        // This is a new interface-only compiler request, not growth of the
        // previous native offer. Retain every old carrier while choosing the
        // newly certified native version for each owner.
        let projection = history_context.compiler_input_projection().interface_only();
        history_context = history_context
            .recover_fixture_projection(projection)
            .unwrap();
        let output = next.compiler_inputs();
        let completed =
            crate::artifact_inventory::CompilerSourceSupport::close_complete_output(output)
                .unwrap();
        let selected_entries = [current_a, current_b]
            .into_iter()
            .map(|product| {
                Arc::new(
                    crate::artifact_inventory::ArtifactEntry::original(
                        canonical_producer,
                        product.clone(),
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let selected_projection =
            crate::artifact_inventory::CompilerInputProjection::from_issued_entries(
                &selected_entries,
            )
            .unwrap();
        let catalog =
            crate::artifact_inventory::NativeDependencyCatalog::from_source(&completed).unwrap();
        let selected_view = selected_projection
            .admit_interfaces_with_native_offers(
                completed.artifacts(),
                selected_entries.clone(),
                completed.native_offers(),
                &catalog,
            )
            .unwrap();
        let selected_support = crate::declaration_context::recover_fixture_compiler_support(
            selected_projection,
            selected_view,
            completed.native_offers().iter().cloned(),
        )
        .unwrap()
        .retain_normalized_availability(completed.native_availability())
        .unwrap();
        history_context = history_context
            .extend_issued_compiler_support(&selected_support)
            .unwrap();
        for entry in &selected_entries {
            assert_eq!(
                history_context.compiler_input_projection().snapshot_roles()
                    [&entry.descriptor.owner]
                    .original(),
                Some(entry.descriptor.id)
            );
            assert_eq!(
                history_context
                    .compiler_source_support()
                    .native_availability()
                    .snapshot_facts()
                    .unwrap()[&entry.descriptor.id]
                    .mode,
                crate::declaration_context::NativeTransportMode::Full
            );
        }
        assert!(history_context
            .artifact_view()
            .selected_native_groups()
            .is_empty());
        assert_eq!(
            history_context
                .compiler_original_products()
                .unwrap()
                .iter()
                .map(|product| product.owner().clone())
                .collect::<BTreeSet<_>>(),
            selected,
        );
        assert_eq!(
            history_context
                .recovery_products()
                .iter()
                .map(|product| product.owner().clone())
                .collect::<BTreeSet<_>>(),
            owner_history,
        );
        latest_recovery = next.data().recovery_products.clone();
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
        for group in product.original_native().groups.iter() {
            assert!(matches!(
                &group.source_imports(product.original_native()).unwrap()[..],
                [PendingImportOwner::Source { owner, original_ordinal, binder: imported }]
                    if owner.module == B
                        && retained_owners.contains(owner)
                        && *original_ordinal == B_ORDINAL
                        && imported == &binder(B, B_ORDINAL)
            ));
        }
    }
    for old in prior.iter() {
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
    assert!(retained_initial_a.original_native().groups.iter().all(|group| {
        matches!(&group.imports[..], [ReceiptImportOwner::Source { unit, module, module_version, .. }] if unit == &initial_b.owner().unit && module == &initial_b.owner().module && module_version.as_ref() == Some(&initial_b.owner().module_version))
    }));
}
