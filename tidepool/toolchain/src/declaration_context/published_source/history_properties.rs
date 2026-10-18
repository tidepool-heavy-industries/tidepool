//! Publication facts remain fixed while the same original gains native demand.
//! The reference model uses authored ordinals, independently of graph queries.
use super::*;
use crate::artifact_inventory::{NativeGroupBinding, NativeGroupKey};
use crate::certified_products::PendingImportOwner;
use proptest::prelude::*;

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

fn fixture(
    initial: usize,
) -> (
    Arc<ArtifactEntry>,
    Arc<ArtifactEntry>,
    Arc<PublishedSourceOriginalSelection>,
) {
    fixture_with_helper_version(initial, 1, false)
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
    let producer = [2; 32];
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
            .find(|row| row.original.descriptor.id == root.descriptor.id)
            .unwrap();
        assert!(ORDINALS.iter().all(|ordinal| row.permits(*ordinal)));
        assert_eq!(
            support.artifacts().selected_native_groups(),
            view.selected_native_groups()
        );
        issued = issued
            .rebuild(|facts| facts.compiler_support = support)
            .finish()
            .unwrap();
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

fn restored_graph_roles(
    context: &ExactDeclarationContext,
    roles: &[CompilerInputRole],
) -> Result<
    (
        tempfile::TempDir,
        RecoveredArtifactInventory,
        ExactDeclarationContext,
    ),
    CompileError,
> {
    let view = context.artifact_view();
    let scratch = tempfile::tempdir().unwrap();
    let (products, interfaces) = context
        .materialize_recovery_products_and_interfaces(scratch.path())
        .unwrap();
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
    let authenticated = inventory
        .context_with_graph_selection(&selection, roles, context.owner.facts.lexical.to_vec())
        .map_err(|error| {
            eprintln!("history recovery stage=graph-role-authentication error={error:?}");
            error
        })?;
    Ok((scratch, inventory, authenticated))
}

fn restored(
    context: &ExactDeclarationContext,
    roles: &[CompilerInputRole],
) -> Result<ExactDeclarationContext, CompileError> {
    // Authenticate the supplied wire before restoring the complete recorded owner.
    let (_scratch, inventory, _authenticated) = restored_graph_roles(context, roles)?;
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
fn publication_reopening_refuses_incomplete_lexical_type_namespace() {
    let (root, helper, publication) = fixture(0);
    let context = publication.context();
    assert!(!context
        .published_source_original_selections()
        .unwrap()
        .is_empty());
    let mut lexical = context.lexical_graph().to_vec();
    lexical
        .iter_mut()
        .find(|node| node.owner == root.descriptor.owner)
        .unwrap()
        .imports
        .clear();
    let mut changed = context.as_ref().clone().rebuild(|facts| {
        facts.lexical = facts.lexical.replace(lexical).unwrap();
    });
    let mut projection = CompilerInputProjection::default();
    for mut role in changed.compiler_input_roles() {
        if role.is_published_source_original() {
            let digest = changed
                .published_selection_sha256(&root.descriptor.owner, &role)
                .unwrap();
            if let CompilerInputRole::PublishedSourceOriginal {
                selection_sha256, ..
            } = &mut role
            {
                *selection_sha256 = digest;
            }
        }
        let owner = changed.artifact_view().metadata_snapshot().artifacts[&role.interface()]
            .descriptor
            .owner
            .clone();
        projection.admit_role(owner, role).unwrap();
    }
    changed = changed.recover_fixture_projection(projection).unwrap();
    let assert_missing_type_role = |context: &ExactDeclarationContext| {
        let Err(CompileError::ArtifactInventory(error)) =
            context.published_source_original_selections()
        else {
            panic!("incomplete publication must refuse its required type role")
        };
        assert_eq!(
            error.failure,
            crate::artifact_inventory::ArtifactInventoryFailure::MissingDependency {
                artifact: root.descriptor.id,
                dependent: root.descriptor.owner.clone(),
                required: helper.descriptor.owner.clone(),
                dependency: crate::artifact_inventory::ArtifactDependency::Interface,
            }
        );
    };
    assert_missing_type_role(&changed);
    // This deliberately malformed fixture authenticates graph/role evidence only;
    // it is not a complete owner for compilation or snapshot continuation.
    let (_scratch, _inventory, recovered) =
        restored_graph_roles(&changed, &changed.compiler_input_roles()).unwrap();
    assert_missing_type_role(&recovered);
}

#[test]
fn generated_publication_support_and_recovery_preserve_issuer_selection() {
    let mut config = proptest::test_runner::contextualize_config(property_config());
    config.source_file =
        Some("tidepool/toolchain/src/declaration_context/published_source/history_properties.rs");
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
    let result = runner.run(&(0..ORDINALS.len(), operations()), |(initial, history)| {
        callbacks.set(callbacks.get() + 1);
        let (root, helper, publication) = fixture_with_helper_version(initial, 1, true);
        let original = publication.context();
        // Publication must carry the earned permission without selecting it.
        let permissions = original.compiler_source_support().native_availability();
        prop_assert!(permissions.rows().find(|row| row.original.descriptor.id == root.descriptor.id)
            .is_some_and(|row| ORDINALS.iter().all(|ordinal| row.permits(*ordinal))));
        prop_assert!(permissions.rows().find(|row| row.original.descriptor.id == helper.descriptor.id)
            .is_some_and(|row| row.permits(7)));
        let expected_publication = original.compiler_input_roles().into_iter()
            .find(CompilerInputRole::is_published_source_original).unwrap();
        // Legacy role DTOs authenticate a selected graph, not the complete v5
        // owner: their selected proof set can omit unused available scopes.
        let wire_roles: Vec<CompilerInputRole> = serde_json::from_slice(
            &serde_json::to_vec(&original.compiler_input_roles()).unwrap(),
        ).unwrap();
        let (_wire_scratch, _wire_inventory, wire_owner) =
            restored_graph_roles(original, &wire_roles).unwrap();
        prop_assert_eq!(wire_owner.artifact_view().capture_graph_selection(),
            original.artifact_view().capture_graph_selection());
        let mut expected_roles = original.compiler_input_roles();
        let mut expected_artifacts = original.artifact_view().artifact_ids();
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
        // Every history reaches the causal interaction, even after shrinking:
        // later same-owner body -> compose publication -> durable reconstruction.
        let history = history
            .into_iter()
            .chain([
                Operation::Demand((initial + 1) % ORDINALS.len()),
                Operation::Compose,
                Operation::Recover,
                Operation::Reopen,
                Operation::DuplicateSelection,
            ])
            .chain((0..7).map(Operation::AlterSelection));
        for operation in history {
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
                        .find(|row| row.original.descriptor.id == root.descriptor.id).unwrap();
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
                            ), [2; 32], Some(BTreeMap::from([(
                                ("fixture".into(), "HistoryRoot".into()), root.descriptor.interface_sha256,
                            )])),
                        ),
                    ]).remove(0).product;
                    let output = Arc::new(ArtifactEntry::original([2; 32], product.clone()).unwrap());
                    let canonical = ArtifactEntry::canonical(product.module_interface().unwrap().clone());
                    let output_role = CompilerInputProjection::from_issued_entries(&[output.clone()]).unwrap().roles();
                    let support = context.compiler_source_support().admit_available_checked_products([2; 32], &[product]).unwrap();
                    context = context.rebuild(|facts| facts.compiler_support = support).finish().unwrap();
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
                    if outputs.insert(ordinal, (output.clone(), output_import, permitted_carriers)).is_none() {
                        expected_roles.extend(output_role);
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
                    context = restored(&context, &roles).unwrap();
                    prop_assert_eq!(context.artifact_view(), &before);
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
                    let issued_roles = context.compiler_input_roles();
                    if partition == 6 {
                        let mut altered = issued_roles.clone();
                        let dependency = altered
                            .iter_mut()
                            .find(|role| role.original() == Some(helper.descriptor.id))
                            .unwrap();
                        *dependency = CompilerInputRole::InterfaceOnly {
                            interface: dependency.interface(),
                        };
                        prop_assert!(restored_graph_roles(&context, &altered).is_err());
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
                            let mut unissued: NativeGroupBinding =
                                serde_json::from_value(groups[0].clone()).unwrap();
                            prop_assert_eq!(unissued.original_ordinal, ORDINALS[initial]);
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
                    prop_assert!(restored_graph_roles(&context, &altered).is_err());
                    refusals[usize::from(partition)]
                        .set(refusals[usize::from(partition)].get() + 1);
                }
                Operation::DuplicateSelection => {
                    // Semantic equality does not admit a noncanonical wire selection.
                    let issued_roles = context.compiler_input_roles();
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
                    prop_assert!(restored_graph_roles(&context, &repeated).is_err());
                    duplicate_refusals.set(duplicate_refusals.get() + 1);
                }
            }
            let actual_roles = context.compiler_input_roles();
            prop_assert_eq!(actual_roles.len(), expected_roles.len());
            prop_assert!(expected_roles.iter().all(|role| actual_roles.contains(role)));
            prop_assert_eq!(context.lexical_graph(), original.lexical_graph());
            let mut actual_artifacts = context.artifact_view().artifact_ids();
            actual_artifacts.sort_unstable();
            let mut expected_artifacts_sorted = expected_artifacts.clone();
            expected_artifacts_sorted.sort_unstable();
            prop_assert_eq!(actual_artifacts, expected_artifacts_sorted);
            // Recorded source choices are independent of later executable demand.
            prop_assert_eq!(context.artifact_view().selected_native_groups(), initial_groups.clone());
            if !outputs.is_empty() {
                // Continue the complete owner, then consume actual exported binders
                // through the same bound-target solver used by compiled targets.
                let request = ProtectedScaffoldRequirements::default().compiler_inputs(&context, None).unwrap();
                let validation = PackageInterfaceValidation::default();
                let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                    &request, &request.metadata, &validation.inventory,
                ).unwrap();
                let entries = outputs.values().map(|(entry, _, _)| entry.clone()).collect();
                let imports = outputs.values().map(|(_, import, _)| import.clone()).collect::<Vec<_>>();
                let inventory = request.artifacts().inventory();
                let mut fixed = inventory.bind_segment(request.artifacts(), entries, &selection).unwrap();
                let target = inventory.admit_bound_target(
                    &mut fixed, request.artifacts(),
                    crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(&imports),
                    vec![], &selection,
                ).unwrap().bind_target(None, imports.clone());
                let groups = target.resolve().unwrap().groups().clone();
                let edges = target.artifacts().binding_dependencies();
                for (ordinal, (output, _, permitted_carriers)) in &outputs {
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
                }
                let expected = demanded.iter().map(|ordinal| NativeGroupKey {
                    artifact: root.descriptor.id, original_ordinal: *ordinal,
                }).chain(demanded.iter().any(|ordinal| *ordinal != 3).then_some(NativeGroupKey {
                    artifact: helper.descriptor.id, original_ordinal: 7,
                })).chain(outputs.values().map(|(entry, _, _)| NativeGroupKey {
                    artifact: entry.descriptor.id, original_ordinal: 0,
                })).collect::<BTreeSet<_>>();
                prop_assert_eq!(target.artifacts().selected_native_groups(), expected);
                let expected_target = outputs.keys().map(|ordinal| NativeGroupKey {
                    artifact: root.descriptor.id, original_ordinal: *ordinal,
                }).chain(outputs.keys().any(|ordinal| *ordinal != 3).then_some(NativeGroupKey {
                    artifact: helper.descriptor.id, original_ordinal: 7,
                })).chain(outputs.values().map(|(entry, _, _)| NativeGroupKey {
                    artifact: entry.descriptor.id, original_ordinal: 0,
                })).collect::<BTreeSet<_>>();
                prop_assert_eq!(groups.iter().map(|group| NativeGroupKey {
                    artifact: group.binding.artifact(), original_ordinal: group.original_ordinal,
                }).collect::<BTreeSet<_>>(), expected_target);
                let issuer_originals = original.compiler_input_roles().iter()
                    .filter_map(CompilerInputRole::original).collect::<BTreeSet<_>>();
                let issued_groups = groups.iter().filter(|group| issuer_originals.contains(&group.binding.artifact()))
                    .all(|group| original.artifact_view().contains_issued_native_group(*group));
                let selected = context.published_source_original_selections_for_target(&groups, &imports).unwrap();
                prop_assert_eq!(selected.len(), usize::from(issued_groups),
                    "target policy follows exact issuer-owned dependency groups: {:?}", groups);
            }
            let Value::Array(rows) = context.published_scope_value().unwrap() else {
                unreachable!()
            };
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
    result.unwrap();
    assert!(duplicate_refusals.get() >= configured_cases as usize);
    assert!(refusals
        .iter()
        .all(|count| count.get() >= configured_cases as usize));
}

#[test]
fn published_role_refuses_missing_issuer_selection() {
    let (_, _, publication) = fixture(0);
    let mut roles = serde_json::to_value(publication.context().compiler_input_roles()).unwrap();
    roles[0].as_object_mut().unwrap().remove("selection");
    assert!(
        serde_json::from_value::<Vec<CompilerInputRole>>(roles).is_err(),
        "legacy publication facts cannot reconstruct the issuer's selection from broader custody"
    );
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn published_source_narrowing_preserves_native_dependency_custody(
        initial in 1usize..ORDINALS.len(), rounds in 1usize..5,
    ) {
        use fvm_ipld_blockstore::Blockstore;
        let (root, helper, publication) = fixture(initial);
        let original = publication.context();
        let expected_graph = original.artifact_view().capture_graph_selection();
        let expected_edges = original.artifact_view().binding_dependencies();
        let expected_role = original.compiler_input_roles().into_iter()
            .find(|role| role.is_published_source_original()).unwrap();
        let helper_role = original.compiler_input_roles().into_iter()
            .find(|role| role.original() == Some(helper.descriptor.id)).unwrap();
        let canonical = helper_role.interface();
        let canonical_binding = *expected_graph.namespace.iter()
            .find(|binding| binding.artifact == canonical).unwrap();
        prop_assert!(expected_graph.native_groups.iter()
            .any(|group| group.binding.artifact() == helper.descriptor.id));
        let owners = BTreeSet::from([root.descriptor.owner.clone()]);
        let mut context = original.as_ref().clone();
        for _ in 0..rounds {
            // Narrow through the owning source operation, preserving its rows,
            // offers and exact graph rather than reconstructing fixture roles.
            let support = context.compiler_source_support().source_support(&owners).unwrap();
            let role = support.projection().roles().into_iter()
                .find(|role| role.interface() == canonical).unwrap();
            prop_assert_eq!(role.original(), None);
            let graph = support.artifacts().capture_graph_selection();
            prop_assert!(graph.namespace.contains(&canonical_binding));
            for binding in &expected_graph.bindings {
                prop_assert!(graph.bindings.contains(binding));
            }
            for group in &expected_graph.native_groups {
                prop_assert!(graph.native_groups.contains(group));
            }
            let edges = support.artifacts().binding_dependencies();
            for edge in &expected_edges {
                prop_assert!(edges.contains(edge));
            }
            context = context.rebuild(|facts| facts.compiler_support = support).finish().unwrap();
            let roles = context.compiler_input_roles();
            prop_assert!(roles.contains(&expected_role));
            prop_assert!(roles.iter().any(|role| role.interface() == canonical && role.original().is_none()));
            let reopened = context.published_source_original_selections().unwrap();
            prop_assert_eq!(reopened.len(), 1);
            prop_assert!(reopened[0].context().compiler_input_roles().contains(&expected_role));

            let store = fvm_ipld_blockstore::MemoryBlockstore::default();
            let mut writer = crate::exact_context::Materializer::new(&store);
            let persisted = writer.materialize_context(None, &context.snapshot()).unwrap();
            for (cid, bytes) in writer.finish().unwrap().into_blocks() {
                store.put_keyed(&cid, &bytes).unwrap();
            }
            let view = context.artifact_view();
            let graph = view.capture_graph_selection();
            let scratch = tempfile::tempdir().unwrap();
            let (products, interfaces) = context
                .materialize_recovery_products_and_interfaces(scratch.path()).unwrap();
            let inventory = RecoveredArtifactInventory::capture_bound(
                scratch.path(), &products, &interfaces, &[], &[],
                &view.descriptors(), &graph.bindings, &view.binding_dependencies(),
            ).unwrap();
            let acquired = crate::exact_context::hydrate_context(&store, persisted.root()).unwrap();
            context = inventory.context_for_expected_snapshot(persisted.root(), &acquired).unwrap();
            prop_assert_eq!(context.compiler_input_roles(), roles);
            prop_assert!(context.published_source_original_selections().unwrap()[0]
                .context().compiler_input_roles().contains(&expected_role));
        }
        // A separately authenticated native version with the same canonical
        // interface is still a real selected-original conflict, not type-only custody.
        let (_, other_helper, other_publication) = fixture_with_helper_version(initial, 2, false);
        prop_assert_eq!(other_helper.descriptor.interface_sha256, helper.descriptor.interface_sha256);
        prop_assert_ne!(other_helper.descriptor.id, helper.descriptor.id);
        let other = other_publication.context().compiler_source_support()
            .source_support(&BTreeSet::from([helper.descriptor.owner.clone()])).unwrap();
        prop_assert!(other.projection().roles().iter()
            .any(|role| role.interface() == canonical
                && role.original() == Some(other_helper.descriptor.id)));
        let before = context.semantic_sha256();
        let Err(CompileError::ExtractFailed(detail)) =
            context.clone().extend_issued_compiler_support(&other)
        else { panic!("a competing selected original must refuse publication reopening") };
        prop_assert!(detail.starts_with("exact declaration context: published selection changes a selected original dependency"));
        prop_assert!(detail.contains(&format!("{:?}", helper.descriptor.owner)), "refusal identifies the dependency owner");
        prop_assert!(detail.contains(&format!("{:?}", helper.descriptor.id)), "refusal identifies the issued original");
        prop_assert!(detail.contains(&format!("{:?}", other_helper.descriptor.id)), "refusal identifies the competing original");
        prop_assert_eq!(context.semantic_sha256(), before);
    }
}
