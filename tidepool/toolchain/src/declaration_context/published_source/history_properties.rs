//! Publication facts remain fixed while the same original gains native demand.
//! The reference model uses authored ordinals, independently of graph queries.
use super::*;
use crate::artifact_inventory::{NativeArtifactDemand, NativeGroupBinding, NativeGroupKey};
use crate::certified_products::PendingImportOwner;
use proptest::prelude::*;
use tidepool_repr::execution_schema::SymbolIdentity;

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
    let producer = [2; 32];
    let helper = Arc::new(
        ArtifactEntry::original(
            producer,
            crate::certified_products::tests::recovered_witness_fixtures(&[
                crate::certified_products::fixture_finalized_product(
                    crate::certified_products::tests::original_groups_fixture_with_interface(
                        "HistoryHelper",
                        vec![(7, vec![])],
                        1,
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
    let helper_import = PendingImportOwner::Source {
        owner: helper_product.owner().clone(),
        original_ordinal: 7,
        binder: SymbolIdentity {
            unit: "fixture".into(),
            module: "HistoryHelper".into(),
            namespace: "value".into(),
            occurrence: "entry_7".into(),
            record_parent: None,
        },
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
    let issued = ExactDeclarationContext::from_authenticated_execution(
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
    let publication = issued
        .issue_published_source_original(
            "history-revision",
            "history-input",
            &root.descriptor.owner,
        )
        .unwrap();
    (root, helper, publication)
}

fn continued_context(
    context: &ExactDeclarationContext,
    view: &ArtifactView,
    root: &ArtifactEntry,
) -> Result<ExactDeclarationContext, CompileError> {
    // Continuations carry the exact issued offers with their compiler roles;
    // fresh selected issuance cannot reconstruct their original proof identity.
    ExactDeclarationContext::from_authenticated_execution_support(
        context.facts.producer,
        crate::artifact_inventory::CompilerSourceSupport::new(
            context.facts.compiler_support.projection.clone(),
            view.clone(),
            context.facts.compiler_support.native_offers.iter().cloned(),
        )?,
        context.facts.lexical.to_vec(),
        root.descriptor.owner.clone(),
        &[root.descriptor.owner.clone()],
    )
}

fn restored(
    context: &ExactDeclarationContext,
    roles: &[CompilerInputRole],
) -> Result<ExactDeclarationContext, CompileError> {
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
    RecoveredArtifactInventory::capture_bound(
        scratch.path(),
        &products,
        &interfaces,
        &[],
        &[],
        &view.descriptors(),
        &selection.bindings,
        &edges,
    )
    .unwrap()
    .context_with_snapshot(&{
        let mut snapshot = context.snapshot();
        snapshot.roles = CompilerInputProjection::restore(view, roles)?
            .snapshot_roles()
            .clone();
        let store = fvm_ipld_blockstore::MemoryBlockstore::default();
        use fvm_ipld_blockstore::Blockstore;
        let mut materializer = crate::exact_context::Materializer::new(&store);
        let retained = materializer
            .materialize_context(None, &snapshot)
            .map_err(failure)?;
        for (cid, bytes) in materializer.finish().map_err(failure)?.into_blocks() {
            store.put_keyed(&cid, &bytes).map_err(failure)?;
        }
        crate::exact_context::hydrate_context(&store, retained.root())
            .map_err(failure)?
            .snapshot()
            .clone()
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
        let (root, helper, publication) = fixture(initial);
        let original = publication.context();
        let expected_publication = original.semantic_sha256();
        let expected_roles = original.compiler_input_roles();
        let expected_artifacts = original.artifact_view().artifact_ids();
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
                    let import = PendingImportOwner::Source {
                        owner: product.owner().clone(),
                        original_ordinal: ordinal,
                        binder: SymbolIdentity {
                            unit: "fixture".into(),
                            module: "HistoryRoot".into(),
                            namespace: "value".into(),
                            occurrence: format!("entry_{ordinal}"),
                            record_parent: None,
                        },
                    };
                    let view = context
                        .facts.compiler_support.artifacts
                        .inventory()
                        .admit_shared_with_demand(
                            context.artifact_view(),
                            vec![root.clone()],
                            NativeArtifactDemand::CertifiedTargetImports(&[import.clone()]),
                        )
                        .unwrap();
                    context = continued_context(&context, &view, &root).unwrap();
                    let imports = [import];
                    let groups = context.artifact_view().native_groups_for_target(None, &imports).unwrap();
                    let issuer_originals = expected_roles.iter().filter_map(CompilerInputRole::original).collect::<BTreeSet<_>>();
                    let issued_groups = groups.iter().filter(|group| issuer_originals.contains(&group.binding.artifact()))
                        .all(|group| original.artifact_view().contains_issued_native_group(*group));
                    let selected = context.published_source_original_selections_for_target(&groups, &imports).unwrap();
                    prop_assert_eq!(selected.len(), usize::from(issued_groups),
                        "later ordinal {} preserves policy only for exact issuer-owned groups: {:?}", ordinal, groups);
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
                    let roles: Vec<CompilerInputRole> = serde_json::from_slice(
                        &serde_json::to_vec(&context.compiler_input_roles()).unwrap(),
                    )
                    .unwrap();
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
                        reopened[0].context().semantic_sha256(),
                        expected_publication
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
                        prop_assert!(restored(&context, &altered).is_err());
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
                    prop_assert!(restored(&context, &altered).is_err());
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
                    let bindings = roles[published]["selection"]["graph"]["bindings"]
                        .as_array_mut()
                        .unwrap();
                    bindings.push(bindings[0].clone());
                    let repeated: Vec<CompilerInputRole> = serde_json::from_value(roles).unwrap();
                    prop_assert_eq!(&repeated, &issued_roles);
                    prop_assert!(restored(&context, &repeated).is_err());
                    duplicate_refusals.set(duplicate_refusals.get() + 1);
                }
            }
            prop_assert_eq!(context.compiler_input_roles(), expected_roles.clone());
            prop_assert_eq!(context.lexical_graph(), original.lexical_graph());
            prop_assert_eq!(
                context.artifact_view().artifact_ids(),
                expected_artifacts.clone()
            );
            prop_assert_eq!(
                context.artifact_view().selected_native_groups(),
                demanded
                    .iter()
                    .map(|ordinal| NativeGroupKey {
                        artifact: root.descriptor.id,
                        original_ordinal: *ordinal,
                    })
                    .chain(
                        demanded
                            .iter()
                            .any(|ordinal| *ordinal != 3)
                            .then_some(NativeGroupKey {
                                artifact: helper.descriptor.id,
                                original_ordinal: 7
                            })
                    )
                    .collect::<BTreeSet<_>>()
            );
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

#[test]
fn unrelated_later_native_offer_does_not_change_reopened_publication() {
    let (root, _, publication) = fixture(0);
    let published = publication.context();
    let original_graph = published.artifact_view().capture_graph_selection();
    let original_roles = published.compiler_input_roles();
    let original_offers = published
        .compiler_source_support()
        .native_offer_observations()
        .unwrap();
    let unrelated = Arc::new(
        ArtifactEntry::original(
            [2; 32],
            crate::certified_products::tests::recovered_witness_fixtures(&[
                crate::certified_products::fixture_finalized_product(
                    crate::certified_products::tests::original_groups_fixture_with_interface(
                        "UnrelatedLaterOutput",
                        vec![(7, vec![])],
                        1,
                        &BTreeMap::new(),
                        b"unrelated later output".to_vec(),
                    ),
                    [2; 32],
                ),
            ])
            .remove(0)
            .product,
        )
        .unwrap(),
    );
    let inventory = ArtifactInventory::default();
    let issued_view = inventory
        .admit_shared(&inventory.empty_view(), vec![unrelated.clone()])
        .unwrap();
    let later_support = crate::artifact_inventory::CompilerSourceSupport::from_selected_issuance(
        CompilerInputProjection::from_issued_entries(&[unrelated.clone()]).unwrap(),
        issued_view,
    )
    .unwrap();
    let carried = published
        .compiler_source_support()
        .merge(&later_support)
        .unwrap();
    assert!(original_offers.is_subset(&carried.native_offer_observations().unwrap()));
    assert!(
        carried.native_offers().len() > published.compiler_source_support().native_offers().len()
    );
    let mut lexical = published.facts.lexical.to_vec();
    lexical.push(ExactLexicalNode {
        owner: unrelated.descriptor.owner.clone(),
        imports: vec![],
    });
    let continued = ExactDeclarationContext::from_authenticated_execution_support(
        [2; 32],
        carried,
        lexical,
        root.descriptor.owner.clone(),
        &[root.descriptor.owner.clone()],
    )
    .unwrap();
    let reopened = continued.published_source_original_selections().unwrap();
    assert_eq!(reopened.len(), 1);
    assert_eq!(
        reopened[0].context().compiler_input_roles(),
        original_roles,
        "the publication's policy graph and policy SHA remain the issued values"
    );
    assert_eq!(
        published.artifact_view().capture_graph_selection(),
        original_graph,
        "later support cannot mutate the original publication view"
    );
    assert_eq!(
        published
            .compiler_source_support()
            .native_offer_observations()
            .unwrap(),
        original_offers,
        "later support cannot mutate original offer issuance"
    );
    assert_eq!(
        reopened[0].context().semantic_sha256(),
        published.semantic_sha256(),
        "unrelated later issuance cannot change the reopened publication's identity"
    );
    assert_eq!(
        reopened[0]
            .context()
            .artifact_view()
            .capture_graph_selection(),
        original_graph
    );
}

#[test]
fn composing_same_publication_preserves_later_executable_demand() {
    let (root, helper, publication) = fixture(0);
    let published = publication.context();
    let ArtifactPayload::Original(product) = &root.payload else {
        unreachable!()
    };
    let import = PendingImportOwner::Source {
        owner: product.owner().clone(),
        original_ordinal: 11,
        binder: SymbolIdentity {
            unit: "fixture".into(),
            module: "HistoryRoot".into(),
            namespace: "value".into(),
            occurrence: "entry_11".into(),
            record_parent: None,
        },
    };
    let view = published
        .artifact_view()
        .inventory()
        .admit_shared_with_demand(
            published.artifact_view(),
            vec![root.clone()],
            NativeArtifactDemand::CertifiedTargetImports(&[import]),
        )
        .unwrap();
    let continued = continued_context(published, &view, &root).unwrap();
    let expected = BTreeSet::from([
        NativeGroupKey {
            artifact: root.descriptor.id,
            original_ordinal: 3,
        },
        NativeGroupKey {
            artifact: root.descriptor.id,
            original_ordinal: 11,
        },
        NativeGroupKey {
            artifact: helper.descriptor.id,
            original_ordinal: 7,
        },
    ]);
    assert_eq!(continued.artifact_view().selected_native_groups(), expected);
    let composed = continued
        .with_published_source_originals(&publication)
        .unwrap();
    assert_eq!(
        composed.compiler_input_roles(),
        published.compiler_input_roles(),
        "the publication policy remains the original issued policy"
    );
    let reopened = composed.published_source_original_selections().unwrap();
    assert_eq!(
        reopened[0].context().semantic_sha256(),
        published.semantic_sha256(),
        "reopening retains the frozen publication independently of current demand"
    );
    assert_eq!(
        composed.artifact_view().selected_native_groups(),
        expected,
        "composing the same source publication does not erase the caller's later executable demand"
    );
}
