//! Actual ordinary and retained-Core issuers preserve nonempty Source imports.

use super::*;
use crate::declaration_context::ExactDeclarationContext;

#[test]
fn retained_core_promotion_rekeys_nonempty_source_import_history() {
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
