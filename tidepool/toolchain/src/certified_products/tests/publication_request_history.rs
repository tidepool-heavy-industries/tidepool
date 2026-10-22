//! Actual Request continuation over publication/recovery histories. The model
//! retains authored ordinals and lexical facts independently of issued graphs.
use super::*;
use crate::artifact_inventory::{
    ArtifactEntry, ArtifactInventory, ArtifactPayload, CanonicalProducerIdentity,
    CompilerInputProjection, CompilerInputRole, NativeGroupBinding, NativeGroupKey,
};
use crate::declaration_context::{
    issue_fixture_compiler_support, ExactCompileContext, ExactDeclarationContext,
    OriginalCompilerInputs, OriginalInstanceEnvironment, PublishedSourceOriginalSelection,
    RecoveredArtifactInventory, RecoveryPayloadReference,
};
use crate::declaration_join::ExactLexicalNode;
use crate::CompileError;
use proptest::prelude::*;
const PRODUCER: &[u8] = b"published-request-history";

const ORDINALS: [u32; 3] = [3, 11, 29];

#[derive(Clone, Debug)]
enum Operation {
    Demand(usize),
    Compose,
    Recover,
    Reopen,
    DuplicateSelection,
    AlterSelection(u8),
}

fn operations() -> impl Strategy<Value = Vec<Operation>> {
    proptest::collection::vec(
        prop_oneof![
            4 => (0..ORDINALS.len()).prop_map(Operation::Demand),
            2 => Just(Operation::Compose),
            2 => Just(Operation::Recover),
            1 => Just(Operation::Reopen),
            1 => (0u8..7).prop_map(Operation::AlterSelection),
        ],
        0..24,
    )
}

fn fixture_with_helper_version(
    initial: usize,
    helper_version: u8,
    full_body_permission: bool,
) -> (
    Arc<ArtifactEntry>,
    Arc<ArtifactEntry>,
    Arc<PublishedSourceOriginalSelection>,
) {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let helper = Arc::new(
        ArtifactEntry::original(
            producer,
            crate::certified_products::tests::recovered_witness_fixtures(&[
                crate::certified_products::fixture_finalized_product(
                    crate::certified_products::tests::original_groups_fixture_with_interface(
                        "HistoryHelper",
                        vec![(7, vec![])],
                        helper_version,
                        &BTreeMap::new(),
                        b"HistoryHelper interface".to_vec(),
                    ),
                    producer,
                ),
            ])
            .remove(0)
            .product,
        )
        .unwrap(),
    );
    let ArtifactPayload::Original(helper_product) = &helper.payload else {
        unreachable!()
    };
    let helper_definitions =
        crate::certified_products::original_native_group_definitions(helper_product, 7).unwrap();
    let helper_binding = match &helper_definitions.bindings()[0] {
        tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
        tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
    };
    let helper_import = PendingImportOwner::Source {
        owner: helper_product.owner().clone(),
        original_ordinal: 7,
        binder: helper_binding.identity.clone(),
    };
    let product = crate::certified_products::tests::recovered_witness_fixtures(&[
        crate::certified_products::fixture_finalized_product_with_requirements(
            crate::certified_products::tests::original_groups_fixture(
                "HistoryRoot",
                ORDINALS
                    .iter()
                    .map(|ordinal| {
                        (
                            *ordinal,
                            if *ordinal == 3 {
                                vec![]
                            } else {
                                vec![helper_import.clone()]
                            },
                        )
                    })
                    .collect(),
                1,
                &BTreeMap::new(),
            ),
            producer,
            Some(BTreeMap::from([(
                ("fixture".into(), "HistoryHelper".into()),
                helper.descriptor.interface_sha256,
            )])),
        ),
    ])
    .remove(0)
    .product;
    let root = Arc::new(ArtifactEntry::original(producer, product).unwrap());
    let target = Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "HistoryTarget",
            BTreeMap::new(),
        ),
    ));
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            vec![root.clone(), helper.clone(), target.clone()],
            &std::iter::once(NativeGroupKey {
                artifact: root.descriptor.id,
                original_ordinal: ORDINALS[initial],
            })
            .chain((initial != 0).then_some(NativeGroupKey {
                artifact: helper.descriptor.id,
                original_ordinal: 7,
            }))
            .collect(),
        )
        .unwrap();
    let mut issued = ExactDeclarationContext::from_authenticated_execution(
        producer,
        &view,
        CompilerInputProjection::from_issued_entries(&[
            root.clone(),
            helper.clone(),
            target.clone(),
        ])
        .unwrap(),
        vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![root.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: root.descriptor.owner.clone(),
                imports: vec![helper.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: helper.descriptor.owner.clone(),
                imports: vec![],
            },
        ],
        target.descriptor.owner.clone(),
        &[
            target.descriptor.owner.clone(),
            root.descriptor.owner.clone(),
            helper.descriptor.owner.clone(),
        ],
    )
    .unwrap();
    if full_body_permission {
        // Earn broad body availability independently of the initial source choice.
        // Calibration below refuses a separately issued, incompatible binding pair.
        let inventory = issued.artifact_view().inventory();
        let entries = issued
            .artifact_view()
            .entries()
            .into_iter()
            .filter(|entry| {
                matches!(&entry.payload, ArtifactPayload::Original(_))
                    || (entry.descriptor.owner != root.descriptor.owner
                        && entry.descriptor.owner != helper.descriptor.owner)
            })
            .collect::<Vec<_>>();
        let all_groups = ORDINALS
            .iter()
            .map(|ordinal| NativeGroupKey {
                artifact: root.descriptor.id,
                original_ordinal: *ordinal,
            })
            .chain(std::iter::once(NativeGroupKey {
                artifact: helper.descriptor.id,
                original_ordinal: 7,
            }))
            .collect();
        let available = inventory
            .admit_recovery_selection(issued.artifact_view(), vec![], &all_groups)
            .unwrap();
        let full = issue_fixture_compiler_support(
            CompilerInputProjection::from_issued_entries(&entries).unwrap(),
            available,
        )
        .unwrap();
        let before = issued.artifact_view().capture_graph_selection();
        let full_graph = full.artifacts().capture_graph_selection();
        for binding in &before.namespace {
            assert!(
                full_graph.namespace.contains(binding),
                "Full support keeps the exact current namespace binding"
            );
        }
        let support = issued
            .compiler_source_support()
            .retain_offers(full.native_offers().iter().cloned())
            .unwrap()
            .retain_normalized_availability(full.native_availability())
            .unwrap();
        let row = support
            .native_availability()
            .rows()
            .find(|row| {
                row.target_original(row.original_owner())
                    .unwrap()
                    .descriptor
                    .id
                    == root.descriptor.id
            })
            .unwrap();
        assert!(ORDINALS.iter().all(|ordinal| row.permits(*ordinal)));
        assert_eq!(
            support.artifacts().selected_native_groups(),
            view.selected_native_groups()
        );
        issued = issued.extend_issued_compiler_support(&support).unwrap();
    }
    let publication = issued
        .issue_published_source_original(
            "history-revision",
            "history-input",
            &root.descriptor.owner,
        )
        .unwrap();
    (root, helper, publication)
}

fn acquire_recovery_inventory(
    context: &ExactDeclarationContext,
) -> (tempfile::TempDir, RecoveredArtifactInventory) {
    let view = context.artifact_view();
    let scratch = tempfile::tempdir().unwrap();
    let mut products = Vec::new();
    let mut interfaces = Vec::new();
    for reference in context
        .materialize_recovery_artifacts(
            scratch.path(),
            &context.artifact_view().artifact_ids().into_iter().collect(),
        )
        .unwrap()
    {
        match reference {
            RecoveryPayloadReference::Original(reference) => products.push(reference),
            RecoveryPayloadReference::Canonical(reference) => interfaces.push(reference),
            RecoveryPayloadReference::Join(_)
            | RecoveryPayloadReference::Value(_)
            | RecoveryPayloadReference::PackageCode(_) => {
                panic!("native-only history contains joined or value interface")
            }
        }
    }
    let selection: crate::artifact_inventory::ArtifactGraphSelection =
        serde_json::from_slice(&serde_json::to_vec(&view.capture_graph_selection()).unwrap())
            .unwrap();
    let edges: Vec<_> =
        serde_json::from_slice(&serde_json::to_vec(&view.binding_dependencies()).unwrap()).unwrap();
    let inventory = RecoveredArtifactInventory::capture_bound(
        scratch.path(),
        &products,
        &interfaces,
        &[],
        &[],
        &view.descriptors(),
        &selection.bindings,
        &edges,
    )
    .expect("history recovery: certificate acquisition");
    (scratch, inventory)
}

fn restored_graph_roles(
    context: &ExactDeclarationContext,
    roles: &[CompilerInputRole],
) -> Result<ExactDeclarationContext, CompileError> {
    let (_scratch, inventory) = acquire_recovery_inventory(context);
    let selection = context.artifact_view().capture_graph_selection();
    let authenticated = inventory
        .context_with_graph_selection(&selection, roles, context.lexical_graph().to_vec())
        .map_err(|error| {
            eprintln!("history recovery stage=graph-role-authentication error={error:?}");
            error
        })?;
    Ok(authenticated)
}

fn restored(context: &ExactDeclarationContext) -> Result<ExactDeclarationContext, CompileError> {
    let (_scratch, inventory) = acquire_recovery_inventory(context);
    use fvm_ipld_blockstore::Blockstore;
    let store = fvm_ipld_blockstore::MemoryBlockstore::default();
    let mut writer = crate::exact_context::Materializer::new(&store);
    let persisted = writer
        .materialize_context(None, &context.snapshot())
        .expect("history recovery: materialize complete v5 owner");
    for (cid, bytes) in writer
        .finish()
        .expect("history recovery: finish v5 blocks")
        .into_blocks()
    {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let acquired = crate::exact_context::hydrate_context(&store, persisted.root())
        .expect("history recovery: hydrate complete v5 owner");
    inventory
        .context_for_expected_snapshot(persisted.root(), &acquired)
        .map_err(|error| {
            eprintln!("history recovery stage=expected-root-full-owner error={error:?}");
            error
        })
}

fn property_config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

#[test]
fn generated_publication_support_and_recovery_preserve_issuer_selection() {
    let mut config = proptest::test_runner::contextualize_config(property_config());
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::generated_publication_support_and_recovery_preserve_issuer_selection"
    ));
    let configured_cases = config.cases;
    let mut runner = proptest::test_runner::TestRunner::new(config);
    let callbacks = std::cell::Cell::new(0usize);
    let observed: [std::cell::Cell<usize>; 5] = std::array::from_fn(|_| std::cell::Cell::new(0));
    let refusals: [std::cell::Cell<usize>; 7] = std::array::from_fn(|_| std::cell::Cell::new(0));
    let duplicate_refusals = std::cell::Cell::new(0usize);
    let carried_reuses = std::cell::Cell::new(0usize);
    let result = runner.run(&(0..ORDINALS.len(), operations()), |(initial, history)| {
        callbacks.set(callbacks.get() + 1);
        let (root, helper, publication) = fixture_with_helper_version(initial, 1, true);
        let original = publication.context();
        // Publication must carry the earned permission without selecting it.
        let permissions = original.compiler_source_support().native_availability();
        prop_assert!(permissions.rows().find(|row| row.target_original(row.original_owner()).unwrap().descriptor.id == root.descriptor.id)
            .is_some_and(|row| ORDINALS.iter().all(|ordinal| row.permits(*ordinal))));
        prop_assert!(permissions.rows().find(|row| row.target_original(row.original_owner()).unwrap().descriptor.id == helper.descriptor.id)
            .is_some_and(|row| row.permits(7)));
        let expected_publication = original.compiler_input_roles().into_iter()
            .find(CompilerInputRole::is_published_source_original).unwrap();
        // Graph-role DTOs describe selected scopes. Calibrate their mutations
        // on an owner without the unused offer scopes carried by the v5 history.
        let (wire_root, wire_helper, wire_publication) =
            fixture_with_helper_version(initial, 1, false);
        let wire_context = wire_publication.context();
        let wire_roles: Vec<CompilerInputRole> = serde_json::from_slice(
            &serde_json::to_vec(&wire_context.compiler_input_roles()).unwrap(),
        ).unwrap();
        let wire_owner = restored_graph_roles(wire_context, &wire_roles).unwrap();
        prop_assert_eq!(wire_owner.artifact_view().capture_graph_selection(),
            wire_context.artifact_view().capture_graph_selection());
        let mut expected_roles = original.compiler_input_roles();
        let mut expected_artifacts = original.artifact_view().artifact_ids();
        let mut expected_lexical = original.lexical_graph().iter()
            .map(|node| (node.owner.clone(), node.clone())).collect::<BTreeMap<_, _>>();
        let initial_groups = std::iter::once(NativeGroupKey {
            artifact: root.descriptor.id,
            original_ordinal: ORDINALS[initial],
        })
        .chain((initial != 0).then_some(NativeGroupKey {
            artifact: helper.descriptor.id,
            original_ordinal: 7,
        }))
        .collect::<BTreeSet<_>>();
        let mut demanded = BTreeSet::from([ORDINALS[initial]]);
        let mut context = original.as_ref().clone();
        let mut outputs = BTreeMap::new();
        let mut output_producers: BTreeMap<u32, (
            CertifiedSourceSelection, OriginalCompilerInputs,
            BTreeSet<NativeGroupBinding>, ProductOrigin,
        )> = BTreeMap::new();
        let mut cached_outputs = BTreeSet::new();
        let request_scratch = tempfile::tempdir().unwrap();
        // Every history reaches the causal interaction, even after shrinking:
        // later same-owner body -> compose publication -> durable reconstruction.
        let history = history
            .into_iter()
            .chain([
                Operation::Demand((initial + 1) % ORDINALS.len()),
                Operation::Demand((initial + 1) % ORDINALS.len()),
                Operation::Demand((initial + 1) % ORDINALS.len()),
                Operation::Compose,
                Operation::Recover,
                Operation::Reopen,
                Operation::DuplicateSelection,
            ])
            .chain((0..7).map(Operation::AlterSelection));
        for (step, operation) in history.enumerate() {
            match operation {
                Operation::Demand(index) => {
                    observed[0].set(observed[0].get() + 1);
                    let ordinal = ORDINALS[index];
                    let ArtifactPayload::Original(product) = &root.payload else {
                        unreachable!()
                    };
                    let definitions = crate::certified_products::original_native_group_definitions(product, ordinal).unwrap();
                    let binding = match &definitions.bindings()[0] {
                        tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
                        tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
                    };
                    let import = PendingImportOwner::Source {
                        owner: product.owner().clone(),
                        original_ordinal: ordinal,
                        binder: binding.identity.clone(),
                    };
                    let row = context.compiler_source_support().native_availability().rows()
                        .find(|row| row.target_original(row.original_owner()).unwrap().descriptor.id == root.descriptor.id).unwrap();
                    prop_assert!(row.permits(ordinal));
                    let dependency = row.dependency_support(NativeGroupKey {
                        artifact: root.descriptor.id, original_ordinal: ordinal,
                    }, product.owner()).unwrap();
                    let permitted_carriers = dependency.groups().map(|group| {
                        let offer = dependency.offer_for_group(group).unwrap();
                        let carriers = offer.issued_group_carriers().unwrap();
                        (group, carriers[&group])
                    }).collect::<BTreeMap<_, _>>();
                    prop_assert!(!permitted_carriers.is_empty());
                    let module = format!("HistoryOutput{ordinal}");
                    let product = crate::certified_products::tests::recovered_witness_fixtures(&[
                        crate::certified_products::fixture_finalized_product_with_requirements(
                            crate::certified_products::tests::original_groups_fixture_with_interface(
                                &module, vec![(0, vec![import.clone()])], 1, &BTreeMap::new(),
                                format!("{module} interface").into_bytes(),
                            ), CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(), Some(BTreeMap::from([(
                                ("fixture".into(), "HistoryRoot".into()), root.descriptor.interface_sha256,
                            )])),
                        ),
                    ]).remove(0).product;
                    let output = Arc::new(ArtifactEntry::original(CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(), product.clone()).unwrap());
                    let canonical = ArtifactEntry::canonical(product.module_interface().unwrap().clone());

                    let request = ExactCompileContext::new(Arc::new(context.clone()))
                        .prepare_compilation(&request_scratch.path().join(format!("input-{step}")), PRODUCER).unwrap();
                    let operation = InventoryOperation::new(Default::default());
                    let mut selection = CertifiedSourceSelection::from_compiler_inputs(
                        request.compiler_inputs(), &operation,
                    ).unwrap();
                    let existing = request.compiler_inputs().projection().snapshot_roles()
                        .get(&output.descriptor.owner);
                    let emitted_before = outputs.contains_key(&ordinal);
                    prop_assert_eq!(existing.is_some(), emitted_before);
                    if let Some(existing) = existing {
                        prop_assert_eq!(existing.interface(), canonical.descriptor.id);
                        prop_assert_eq!(existing.original(), cached_outputs.contains(&ordinal)
                            .then_some(output.descriptor.id));
                        prop_assert!(request.compiler_inputs().native_availability(output.descriptor.id)
                            .is_some_and(|row| row.permits(0)));
                    }
                    let origin = if emitted_before { ProductOrigin::Cached } else { ProductOrigin::RetainedCore };
                    let expected_output_role = if emitted_before {
                        CompilerInputRole::ReusableOriginal { interface: canonical.descriptor.id, original: output.descriptor.id }
                    } else {
                        CompilerInputRole::InterfaceOnly { interface: canonical.descriptor.id }
                    };
                    selection.admit_current(product.owner(), origin,
                        product.module_interface().unwrap(), &operation).unwrap();
                    selection.bind_produced_originals(
                        &IssuedGroupCandidates::bind(Arc::from([]), std::slice::from_ref(&product)).unwrap(),
                    ).unwrap();
                    let completed = crate::declaration_context::CapturedCompilerStage::issue(
                        &mut selection, request.compiler_inputs().artifacts(), vec![output.clone()],
                        &mut PackageInterfaceValidation::default(),
                    ).unwrap();
                    let actual_role = &completed.compiler_inputs().role_projection().snapshot_roles()[&output.descriptor.owner];
                    prop_assert_eq!(actual_role, &expected_output_role,
                        "completed output role: step={} ordinal={} owner={:?} origin={:?} input_role={:?}",
                        step, ordinal, output.descriptor.owner, origin, existing);
                    let output_permission = completed.compiler_inputs().native_availability().rows()
                        .find(|row| row.original_owner() == product.owner()).unwrap();
                    prop_assert!(output_permission.permits(0));
                    prop_assert_eq!(completed.compiler_inputs().native_availability().snapshot_facts().unwrap()[&output.descriptor.id].mode.clone(),
                        crate::declaration_context::NativeTransportMode::Full);
                    prop_assert!(!completed.issued_artifacts().selected_native_groups().iter()
                        .any(|group| group.artifact == output.descriptor.id));
                    let produced = selection.produced_source_receipt(
                        &output.descriptor.owner, completed.issued_artifacts(),
                    ).unwrap();
                    let new_bindings = if let Some(own) = produced {
                        Some(own.own_offer().issued_group_carriers().unwrap()
                            .into_keys().collect::<BTreeSet<_>>())
                    } else {
                        prop_assert!(emitted_before && cached_outputs.contains(&ordinal));
                        let (prior_selection, _, prior_bindings, _) = output_producers.get(&ordinal)
                            .expect("carried Cached input must retain its actual earlier producer");
                        let prior = prior_selection.target_original(product.owner()).unwrap().unwrap();
                        prop_assert_eq!(prior.as_ref(), output.as_ref());
                        let ArtifactPayload::Original(prior_product) = &prior.payload else { unreachable!() };
                        prop_assert_eq!(prior_product.product_bytes(), product.product_bytes());
                        prop_assert_eq!(prior_product.certification_bytes(), product.certification_bytes());
                        prop_assert_eq!(prior_product.module_interface(), product.module_interface());
                        let key = NativeGroupKey { artifact: output.descriptor.id, original_ordinal: 0 };
                        prop_assert!(selection.current_product(key, &output).unwrap().is_none());
                        let carried = output_permission.dependency_support(key, product.owner()).unwrap()
                            .groups().collect::<BTreeSet<_>>();
                        let prior_own = prior_bindings.iter().filter(|group|
                            group.binding.artifact() == output.descriptor.id && group.original_ordinal == 0)
                            .copied().collect::<BTreeSet<_>>();
                        prop_assert_eq!(prior_own.len(), 1);
                        prop_assert!(prior_own.is_subset(&carried));
                        carried_reuses.set(carried_reuses.get() + 1);
                        None
                    };
                    let support = crate::artifact_inventory::CompilerSourceSupport::close_complete_output(completed.compiler_inputs()).unwrap();
                    context = context.extend_issued_compiler_support(&support).unwrap();
                    if let Some(bindings) = new_bindings {
                        output_producers.insert(ordinal, (selection, completed, bindings, origin));
                    }
                    prop_assert_eq!(&output.requirements, &vec![root.descriptor.owner.clone()]);
                    let output_lexical = ExactLexicalNode {
                        owner: output.descriptor.owner.clone(),
                        imports: vec![root.descriptor.owner.clone()],
                    };
                    let mut lexical = context.lexical_graph().to_vec();
                    lexical.push(output_lexical.clone());
                    context = context.extend_lexical_joins(&[], &lexical).unwrap();
                    expected_lexical.insert(output_lexical.owner.clone(), output_lexical);
                    let definitions = crate::certified_products::original_native_group_definitions(
                        match &output.payload { ArtifactPayload::Original(product) => product, _ => unreachable!() }, 0,
                    ).unwrap();
                    let binding = match &definitions.bindings()[0] {
                        tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
                        tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
                    };
                    let output_import = PendingImportOwner::Source {
                        owner: match &output.payload { ArtifactPayload::Original(product) => product.owner().clone(), _ => unreachable!() },
                        original_ordinal: 0,
                        binder: binding.identity.clone(),
                    };
                    if emitted_before { cached_outputs.insert(ordinal); }
                    expected_roles.retain(|role| role.interface() != canonical.descriptor.id);
                    expected_roles.push(expected_output_role);
                    if outputs.insert(ordinal, (output.clone(), output_import, permitted_carriers)).is_none() {
                        expected_artifacts.extend([output.descriptor.id, canonical.descriptor.id]);
                    }
                    demanded.insert(ordinal);
                }
                Operation::Compose => {
                    observed[1].set(observed[1].get() + 1);
                    context = context
                        .with_published_source_originals(&publication)
                        .unwrap();
                }
                Operation::Recover => {
                    observed[2].set(observed[2].get() + 1);
                    let before = context.artifact_view().clone();
                    let roles = context.compiler_input_roles();
                    context = restored(&context).unwrap();
                    prop_assert_eq!(context.artifact_view(), &before);
                    prop_assert_eq!(context.compiler_input_roles(), roles);
                    prop_assert_eq!(
                        context.original_instance_environment(),
                        &OriginalInstanceEnvironment::Unknown
                    );
                }
                Operation::Reopen => {
                    observed[3].set(observed[3].get() + 1);
                    let reopened = context.published_source_original_selections().unwrap();
                    prop_assert_eq!(reopened.len(), 1);
                    prop_assert_eq!(
                        reopened[0].context().compiler_input_roles().into_iter()
                            .find(CompilerInputRole::is_published_source_original).unwrap(),
                        expected_publication.clone()
                    );
                    prop_assert_eq!(
                        reopened[0]
                            .context()
                            .artifact_view()
                            .selected_native_groups(),
                        initial_groups.clone()
                    );
                }
                Operation::AlterSelection(partition) => {
                    observed[4].set(observed[4].get() + 1);
                    let issued_roles = wire_context.compiler_input_roles();
                    if partition == 6 {
                        let mut altered = issued_roles.clone();
                        let dependency = altered
                            .iter_mut()
                            .find(|role| role.original() == Some(wire_helper.descriptor.id))
                            .unwrap();
                        *dependency = CompilerInputRole::InterfaceOnly {
                            interface: dependency.interface(),
                        };
                        prop_assert!(restored_graph_roles(wire_context, &altered).is_err());
                        refusals[6].set(refusals[6].get() + 1);
                        continue;
                    }
                    let published = issued_roles
                        .iter()
                        .position(CompilerInputRole::is_published_source_original)
                        .unwrap();
                    let mut roles = serde_json::to_value(&issued_roles).unwrap();
                    let issued = roles[published].clone();
                    let graph = &mut roles[published]["selection"]["graph"];
                    match partition {
                        0 => graph["native_groups"].as_array_mut().unwrap().clear(),
                        1 => {
                            let groups = graph["native_groups"].as_array_mut().unwrap();
                            let roots = groups.iter().map(|group|
                                serde_json::from_value::<NativeGroupBinding>(group.clone()).unwrap())
                                .filter(|group| group.binding.artifact() == wire_root.descriptor.id
                                    && group.original_ordinal == ORDINALS[initial])
                                .collect::<Vec<_>>();
                            prop_assert_eq!(roots.len(), 1, "mutation names the exact selected Root, independently of helper row order");
                            let mut unissued = roots[0];
                            unissued.original_ordinal = ORDINALS[(initial + 1) % ORDINALS.len()];
                            groups.push(serde_json::to_value(unissued).unwrap());
                        }
                        2 => graph["bindings"].as_array_mut().unwrap().clear(),
                        3 => {
                            let bindings = graph["bindings"].as_array_mut().unwrap();
                            let scope = bindings[0]["selection"].as_array_mut().unwrap();
                            scope[0] = serde_json::json!(scope[0].as_u64().unwrap() ^ 1);
                        }
                        4 => graph["namespace"].as_array_mut().unwrap().clear(),
                        5 => {
                            let digest =
                                roles[published]["selection_sha256"].as_array_mut().unwrap();
                            digest[0] = serde_json::json!(digest[0].as_u64().unwrap() ^ 1);
                        }
                        _ => unreachable!(),
                    }
                    prop_assert_ne!(&roles[published], &issued);
                    let altered: Vec<CompilerInputRole> = serde_json::from_value(roles).unwrap();
                    prop_assert!(restored_graph_roles(wire_context, &altered).is_err());
                    refusals[usize::from(partition)]
                        .set(refusals[usize::from(partition)].get() + 1);
                }
                Operation::DuplicateSelection => {
                    // Semantic equality does not admit a noncanonical wire selection.
                    let issued_roles = wire_context.compiler_input_roles();
                    let published = issued_roles
                        .iter()
                        .position(CompilerInputRole::is_published_source_original)
                        .unwrap();
                    let mut roles = serde_json::to_value(&issued_roles).unwrap();
                    let baseline_wire: Vec<CompilerInputRole> = serde_json::from_value(roles.clone()).unwrap();
                    let bindings = roles[published]["selection"]["graph"]["bindings"]
                        .as_array_mut()
                        .unwrap();
                    bindings.push(bindings[0].clone());
                    let repeated: Vec<CompilerInputRole> = serde_json::from_value(roles).unwrap();
                    prop_assert_eq!(&repeated, &baseline_wire);
                    prop_assert!(restored_graph_roles(wire_context, &repeated).is_err());
                    duplicate_refusals.set(duplicate_refusals.get() + 1);
                }
            }
            let actual_roles = context.compiler_input_roles();
            prop_assert_eq!(actual_roles.len(), expected_roles.len());
            prop_assert!(expected_roles.iter().all(|role| actual_roles.contains(role)));
            prop_assert_eq!(context.lexical_graph().iter()
                .map(|node| (node.owner.clone(), node.clone())).collect::<BTreeMap<_, _>>(),
                expected_lexical.clone());
            prop_assert_eq!(actual_roles.iter().find(|role| role.is_published_source_original()).unwrap(),
                &expected_publication);
            let mut actual_artifacts = context.artifact_view().artifact_ids();
            actual_artifacts.sort_unstable();
            let mut expected_artifacts_sorted = expected_artifacts.clone();
            expected_artifacts_sorted.sort_unstable();
            prop_assert_eq!(actual_artifacts, expected_artifacts_sorted);
            // Recorded source choices are independent of later executable demand.
            prop_assert_eq!(context.artifact_view().selected_native_groups(), initial_groups.clone());
            if !outputs.is_empty() {
                // Each emission has its own physical producer. Recovery of the
                // context preserves availability, not same-program Source identity.
                let mut aggregate_targets = BTreeSet::new();
                for (ordinal, (output, _, permitted_carriers)) in &outputs {
                    let (selection, physical, expected_bindings, origin) = &output_producers[ordinal];
                    let ArtifactPayload::Original(product) = &output.payload else { unreachable!() };
                    let mut observations = source_target_observations(&[(product, 0)]);
                    if *origin == ProductOrigin::RetainedCore {
                        let ReceiptImportOwner::Source { module_version, .. } = &mut observations[0].owner
                            else { unreachable!() };
                        *module_version = None;
                    }
                    let prepared = prepared_target_fixture(&observations);
                    let inventory = physical.issued_artifacts().inventory();
                    let baseline = physical.issued_artifacts().capture_graph_selection().native_groups
                        .into_iter().collect::<BTreeSet<_>>();
                    let before_target = inventory.metrics().issued_scope_nodes;
                    let mut validation = PackageInterfaceValidation::default();
                    let observed = AuthenticatedTargetImports::issue(
                        &prepared, &observations, &validation.inventory,
                    ).unwrap();
                    let target = inventory.select_native_target(
                        physical.physical_cohort(), selection,
                        observed, None, None, &[], &BTreeMap::new(), &mut validation,
                    ).unwrap();
                    let groups = target.resolve().unwrap().groups().clone();
                    prop_assert_eq!(&groups, expected_bindings);
                    prop_assert_eq!(inventory.metrics().issued_scope_nodes, before_target);
                    let edges = target.artifacts().binding_dependencies();
                    prop_assert!(edges.iter().any(|(dependent, required, edge)| {
                        matches!((dependent, required), (
                            crate::artifact_inventory::ArtifactBindingNode::Group(dependent),
                            crate::artifact_inventory::ArtifactBindingNode::Group(required),
                        ) if dependent.binding.artifact() == output.descriptor.id
                            && dependent.original_ordinal == 0
                            && permitted_carriers.get(required).is_some_and(|carrier| edges.iter().any(|(source, target, edge)| {
                                source == &crate::artifact_inventory::ArtifactBindingNode::Group(*required)
                                    && target == &crate::artifact_inventory::ArtifactBindingNode::Artifact(*carrier)
                                    && matches!(edge, crate::artifact_inventory::ArtifactDependency::Interface)
                            })))
                            && matches!(edge, crate::artifact_inventory::ArtifactDependency::NativeGroup {
                                dependent_ordinal: 0, required_ordinal,
                            } if required_ordinal == ordinal)
                    }), "target output requires the previously permitted Root group with its issuing carrier edge: expected={:?}, actual={:?}", permitted_carriers, edges);
                    let expected_target = std::iter::once(NativeGroupKey {
                        artifact: root.descriptor.id, original_ordinal: *ordinal,
                    }).chain((*ordinal != 3).then_some(NativeGroupKey {
                        artifact: helper.descriptor.id, original_ordinal: 7,
                    })).chain(std::iter::once(NativeGroupKey {
                        artifact: output.descriptor.id, original_ordinal: 0,
                    })).collect::<BTreeSet<_>>();
                    let expected_retained = baseline.union(expected_bindings)
                        .copied().collect::<BTreeSet<_>>();
                    prop_assert_eq!(target.artifacts().capture_graph_selection().native_groups
                        .into_iter().collect::<BTreeSet<_>>(), expected_retained);
                    let actual_target = groups.iter().map(|group| NativeGroupKey {
                        artifact: group.binding.artifact(), original_ordinal: group.original_ordinal,
                    }).collect::<BTreeSet<_>>();
                    prop_assert_eq!(&actual_target, &expected_target);
                    aggregate_targets.extend(actual_target);
                    let issuer_originals = original.compiler_input_roles().iter()
                        .filter_map(CompilerInputRole::original).collect::<BTreeSet<_>>();
                    let issued_groups = groups.iter().filter(|group| issuer_originals.contains(&group.binding.artifact()))
                        .all(|group| original.artifact_view().contains_issued_native_group(*group));
                    let selected = context.published_source_original_selections_for_target(&groups, target.imports()).unwrap();
                    prop_assert_eq!(selected.len(), usize::from(issued_groups),
                        "target policy follows exact issuer-owned dependency groups: {:?}", groups);
                }
                let expected_targets = outputs.keys().map(|ordinal| NativeGroupKey {
                    artifact: root.descriptor.id, original_ordinal: *ordinal,
                }).chain(outputs.keys().any(|ordinal| *ordinal != 3).then_some(NativeGroupKey {
                    artifact: helper.descriptor.id, original_ordinal: 7,
                })).chain(outputs.values().map(|(entry, _, _)| NativeGroupKey {
                    artifact: entry.descriptor.id, original_ordinal: 0,
                })).collect::<BTreeSet<_>>();
                prop_assert_eq!(&aggregate_targets, &expected_targets);
                let expected_history = demanded.iter().map(|ordinal| NativeGroupKey {
                    artifact: root.descriptor.id, original_ordinal: *ordinal,
                }).chain(demanded.iter().any(|ordinal| *ordinal != 3).then_some(NativeGroupKey {
                    artifact: helper.descriptor.id, original_ordinal: 7,
                })).chain(outputs.values().map(|(entry, _, _)| NativeGroupKey {
                    artifact: entry.descriptor.id, original_ordinal: 0,
                })).collect::<BTreeSet<_>>();
                prop_assert_eq!(initial_groups.union(&aggregate_targets).copied()
                    .collect::<BTreeSet<_>>(), expected_history);
            }
            let exact_request = ExactCompileContext::new(Arc::new(context.clone()))
                .prepare_compilation(&request_scratch.path().join(format!("published-wire-{step}")), PRODUCER).unwrap();
            let bytes = std::fs::read(&exact_request.manifest).unwrap();
            let scope: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
            let scope = scope.as_array().unwrap();
            prop_assert_eq!(scope.len(), 11);
            prop_assert_eq!(&scope[0], &Value::Text("TPEXACTSCOPE".into()));
            prop_assert_eq!(&scope[1], &Value::Text("16".into()));
            let Value::Array(rows) = &scope[9] else { unreachable!() };
            let Value::Array(row) = &rows[0] else {
                unreachable!()
            };
            let Value::Array(dependencies) = &row[7] else {
                unreachable!()
            };
            prop_assert_eq!(dependencies.len(), 1);
            prop_assert_eq!(
                context.compiler_metadata_snapshot().unwrap().entries[&helper.descriptor.owner]
                    .descriptor
                    .id,
                helper.descriptor.id
            );
        }
        prop_assert!(demanded.len() >= 2);
        Ok(())
    });
    eprintln!(
        "publication history configured_cases={configured_cases} callbacks={} demand={} compose={} recovery={} reopen={} refusal={}",
        callbacks.get(),
        observed[0].get(),
        observed[1].get(),
        observed[2].get(),
        observed[3].get(),
        observed[4].get()
    );
    eprintln!(
        "publication refusal cases missing_groups={} unissued_group={} missing_bindings={} wrong_binding_scope={} wrong_namespace={} wrong_digest={} changed_dependency_role={}",
        refusals[0].get(),
        refusals[1].get(),
        refusals[2].get(),
        refusals[3].get(),
        refusals[4].get(),
        refusals[5].get(),
        refusals[6].get()
    );
    eprintln!(
        "publication duplicate-row refusals={}",
        duplicate_refusals.get()
    );
    eprintln!(
        "publication carried producer reuses={}",
        carried_reuses.get()
    );
    result.unwrap();
    assert!(duplicate_refusals.get() >= configured_cases as usize);
    assert!(carried_reuses.get() >= configured_cases as usize);
    assert!(refusals
        .iter()
        .all(|count| count.get() >= configured_cases as usize));
}
