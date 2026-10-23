//! Publication recovery and narrowing preserve exact selected native and type custody.
use super::*;
use crate::artifact_inventory::NativeGroupKey;
use crate::certified_products::PendingImportOwner;
use proptest::prelude::*;

const ORDINALS: [u32; 3] = [3, 11, 29];

fn retained_fixture(
    initial: usize,
) -> (
    Arc<ArtifactEntry>,
    Arc<ArtifactEntry>,
    ExactDeclarationContext,
) {
    retained_fixture_with_helper_version(initial, 1)
}

fn retained_fixture_with_helper_version(
    initial: usize,
    helper_version: u8,
) -> (
    Arc<ArtifactEntry>,
    Arc<ArtifactEntry>,
    ExactDeclarationContext,
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
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            vec![root.clone(), helper.clone()],
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
    let issued = ExactDeclarationContext::retained_execution_fixture(
        producer,
        &view,
        CompilerInputProjection::from_issued_entries(&[root.clone(), helper.clone()]).unwrap(),
        vec![
            ExactLexicalNode {
                owner: root.descriptor.owner.clone(),
                imports: vec![helper.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: helper.descriptor.owner.clone(),
                imports: vec![],
            },
        ],
        root.descriptor.owner.clone(),
        &[
            root.descriptor.owner.clone(),
            helper.descriptor.owner.clone(),
        ],
    )
    .unwrap();
    (root, helper, issued)
}

#[test]
#[ignore = "requires the declared rooted compiler service"]
fn publication_reopening_keeps_birth_when_current_roles_change() {
    let fixture = super::tests::compiled_publication_fixture();
    let root = &fixture.root;
    let helper = &fixture.helper;
    let publication = &fixture.publication;
    let source = publication.context();
    let support = source
        .compiler_source_support()
        .source_support(&BTreeSet::from([root.descriptor.owner.clone()]))
        .unwrap();
    let source = source
        .as_ref()
        .clone()
        .rebuild(|facts| facts.compiler_support = support)
        .finish()
        .unwrap();
    let before = publication
        .context()
        .compiler_input_projection()
        .snapshot_roles()[&root.descriptor.owner]
        .clone();
    assert!(
        source.compiler_input_projection().snapshot_roles()[&helper.descriptor.owner]
            .original()
            .is_none()
    );
    let reopened = source.published_source_original_selections().unwrap();
    assert_eq!(reopened.len(), 1);
    assert_eq!(
        reopened[0]
            .context()
            .compiler_input_projection()
            .snapshot_roles()[&root.descriptor.owner],
        before
    );
    assert!(reopened[0]
        .context()
        .compiler_input_projection()
        .snapshot_roles()[&helper.descriptor.owner]
        .original()
        .is_none());
    let reissued = fixture
        .entry
        .products()
        .published_source_original_selection("history-revision", &root.descriptor.owner)
        .unwrap();
    assert_eq!(
        reissued
            .context()
            .compiler_input_projection()
            .snapshot_roles()[&root.descriptor.owner],
        before
    );
    let alternative = fixture
        .entry
        .products()
        .published_source_original_selection("conflicting-revision", &root.descriptor.owner)
        .unwrap();
    assert!(source
        .with_published_source_originals(&alternative)
        .is_err());
}

pub(super) fn restored_graph_roles(
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
    let references = context.materialize_recovery_artifacts(
        scratch.path(),
        &context.artifact_view().artifact_ids().into_iter().collect(),
    )?;
    let selection: crate::artifact_inventory::ArtifactGraphSelection =
        serde_json::from_slice(&serde_json::to_vec(&view.capture_graph_selection()).unwrap())
            .unwrap();
    let edges: Vec<_> =
        serde_json::from_slice(&serde_json::to_vec(&view.binding_dependencies()).unwrap()).unwrap();
    let inventory = RecoveredArtifactAcquisition::new(scratch.path())
        .capture_payloads_bound(
            &references,
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
#[ignore = "requires the declared rooted compiler service"]
fn publication_reopening_refuses_missing_type_namespace_with_retained_custody() {
    let fixture = super::tests::compiled_publication_fixture();
    let (root, helper, publication) = (&fixture.root, &fixture.helper, &fixture.publication);
    let context = publication.context();
    assert!(!context
        .published_source_original_selections()
        .unwrap()
        .is_empty());
    // Keep the genuine lexical and source evidence unchanged. Removing a
    // namespace role below must fail despite retaining its certified payload.
    let changed = context.as_ref().clone();
    let assert_missing_type_role = |result: Result<ExactDeclarationContext, CompileError>| {
        let Err(CompileError::ArtifactInventory(error)) = result else {
            panic!("incomplete publication must refuse its required type role: {result:?}")
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
    let mut incomplete = CompilerInputProjection::default();
    for (owner, role) in changed.compiler_input_projection().snapshot_roles() {
        if owner != &helper.descriptor.owner {
            incomplete.admit_role(owner.clone(), role.clone()).unwrap();
        }
    }
    let canonical =
        context.compiler_input_projection().snapshot_roles()[&helper.descriptor.owner].interface();
    let mut graph = changed.artifact_view().capture_graph_selection();
    graph.namespace.retain(|binding| {
        binding.artifact != canonical && binding.artifact != helper.descriptor.id
    });
    let view = changed
        .artifact_view()
        .select_issued(
            vec![root.descriptor.id],
            &crate::artifact_inventory::ExactArtifactSelection::from_recovered_graph_selection(
                graph.clone(),
            ),
        )
        .unwrap();
    assert_missing_type_role(
        recover_fixture_compiler_support(
            incomplete,
            view,
            changed
                .compiler_source_support()
                .native_offers()
                .iter()
                .cloned(),
        )
        .and_then(|support| {
            changed
                .clone()
                .rebuild(|facts| facts.compiler_support = support)
                .finish()
        }),
    );
    let (_scratch, inventory, _) =
        restored_graph_roles(&changed, &changed.compiler_input_roles()).unwrap();
    let mut snapshot = changed.snapshot();
    snapshot.native_fact_owner = None;
    snapshot.roles.remove(&helper.descriptor.owner);
    snapshot.selection =
        Arc::new(crate::exact_context::ExactSelectionSnapshot::from_graph_selection(&graph));
    let store = fvm_ipld_blockstore::MemoryBlockstore::default();
    let mut writer = crate::exact_context::Materializer::new(&store);
    let written = writer.materialize_context(None, &snapshot).unwrap();
    use fvm_ipld_blockstore::Blockstore;
    for (cid, bytes) in writer.finish().unwrap().into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let decoded = crate::exact_context::hydrate_context(&store, written.root()).unwrap();
    assert_missing_type_role(inventory.context_for_expected_snapshot(written.root(), &decoded));
}

#[test]
#[ignore = "requires the declared rooted compiler service"]
fn published_role_refuses_missing_issuer_selection() {
    let fixture = super::tests::compiled_publication_fixture();
    let publication = &fixture.publication;
    let recorded = publication
        .context()
        .snapshot()
        .recorded_rows()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut roles = serde_json::to_value(&recorded).unwrap();
    assert_eq!(
        serde_json::from_value::<Vec<crate::exact_context::RecordedCompilerRow>>(roles.clone())
            .unwrap(),
        recorded
    );
    let published = recorded
        .iter()
        .position(|row| row.publication().is_some())
        .expect("fixture publication carries an exact source selection");
    roles[published]["provenance"]["birth"]
        .as_object_mut()
        .unwrap()
        .remove("selection");
    assert!(
        serde_json::from_value::<Vec<crate::exact_context::RecordedCompilerRow>>(roles).is_err(),
        "legacy publication facts cannot reconstruct the issuer's selection from broader custody"
    );
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn retained_source_narrowing_preserves_native_dependency_custody(
        initial in 1usize..ORDINALS.len(), rounds in 1usize..5, helper_version in 2u8..5,
    ) {
        use fvm_ipld_blockstore::Blockstore;
        let (root, helper, retained) = retained_fixture(initial);
        let original = &retained;
        let expected_graph = original.artifact_view().capture_graph_selection();
        let expected_edges = original.artifact_view().binding_dependencies();
        let expected_role = original.compiler_input_projection().snapshot_roles()[&root.descriptor.owner].clone();
        let helper_role = original.compiler_input_roles().into_iter()
            .find(|role| role.original() == Some(helper.descriptor.id)).unwrap();
        let canonical = helper_role.interface();
        let canonical_binding = *expected_graph.namespace.iter()
            .find(|binding| binding.artifact == canonical).unwrap();
        prop_assert!(expected_graph.native_groups.iter()
            .any(|group| group.binding.artifact() == helper.descriptor.id));
        let owners = BTreeSet::from([root.descriptor.owner.clone()]);
        let mut context = original.clone();
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

            let store = fvm_ipld_blockstore::MemoryBlockstore::default();
            let mut writer = crate::exact_context::Materializer::new(&store);
            let persisted = writer.materialize_context(None, &context.snapshot()).unwrap();
            for (cid, bytes) in writer.finish().unwrap().into_blocks() {
                store.put_keyed(&cid, &bytes).unwrap();
            }
            let view = context.artifact_view();
            let graph = view.capture_graph_selection();
            let scratch = tempfile::tempdir().unwrap();
            let (products, interfaces) = partition_native_recovery_references(
                context.materialize_recovery_artifacts(
                    scratch.path(),
                    &context.artifact_view().artifact_ids().into_iter().collect(),
                ).unwrap(),
            );
            let inventory = RecoveredArtifactInventory::capture_bound(
                scratch.path(), &products, &interfaces, &[], &[],
                &view.descriptors(), &graph.bindings, &view.binding_dependencies(),
            ).unwrap();
            let acquired = crate::exact_context::hydrate_context(&store, persisted.root()).unwrap();
            context = inventory.context_for_expected_snapshot(persisted.root(), &acquired).unwrap();
            prop_assert_eq!(context.compiler_input_roles(), roles);

        }
        // A separately authenticated native version with the same canonical
        // interface is still a real selected-original conflict, not type-only custody.
        let (other_root, other_helper, other_retained) = retained_fixture_with_helper_version(initial, helper_version);
        prop_assert_eq!(other_helper.descriptor.interface_sha256, helper.descriptor.interface_sha256);
        prop_assert_ne!(other_helper.descriptor.id, helper.descriptor.id);
        prop_assert_ne!(other_root.descriptor.id, root.descriptor.id);
        let other = other_retained.compiler_source_support()
            .source_support(&BTreeSet::from([helper.descriptor.owner.clone()])).unwrap();
        prop_assert!(other.projection().roles().iter()
            .any(|role| role.interface() == canonical
                && role.original() == Some(other_helper.descriptor.id)));
        let before = context.semantic_sha256();
        let ArtifactPayload::Original(other_product) = &other_root.payload else { unreachable!() };
        let canonical = ArtifactEntry::canonical_artifact_id(other_product.module_interface());
        let crossed = context.compiler_source_support().checked_native_input_decision(
            other_root.descriptor.id, canonical, other_product.owner(),
        ).map(|_| ());
        prop_assert!(matches!(crossed,
            Err(crate::certified_products::InputDecisionRefusal::ArtifactIdentityMismatch(ref mismatch))
                if mismatch.stage == crate::certified_products::ArtifactIdentityStage::RequestedIdentityDiffers
                    && mismatch.expected_original == Some(other_root.descriptor.id)
                    && mismatch.observed_original == Some(root.descriptor.id)),
            "historical group custody cannot replace the selected root: {:?}", crossed);
        prop_assert_eq!(context.semantic_sha256(), before);
    }
}

#[test]
#[ignore = "requires the declared rooted compiler service"]
fn published_source_narrowing_preserves_native_dependency_custody() {
    use fvm_ipld_blockstore::Blockstore;
    let fixture = super::tests::compiled_publication_fixture();
    let original = fixture.publication.context();
    let expected_graph = original.artifact_view().capture_graph_selection();
    let expected_edges = original.artifact_view().binding_dependencies();
    let root_owner = &fixture.root.descriptor.owner;
    let helper_owner = &fixture.helper.descriptor.owner;
    let expected_role = original.compiler_input_projection().snapshot_roles()[root_owner].clone();
    let canonical = original.compiler_input_projection().snapshot_roles()[helper_owner].interface();
    assert!(
        expected_graph
            .native_groups
            .iter()
            .any(|group| group.binding.artifact() == fixture.helper.descriptor.id),
        "the real second export depends on its captured Helper native body"
    );
    // One sealed producer owns the seed. Each generated history receives fresh
    // retained facts and may only narrow, transfer, or restore that issuance.
    let mut config = proptest::test_runner::contextualize_config(property_config());
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::published_source_narrowing_preserves_native_dependency_custody"
    ));
    let mut runner = proptest::test_runner::TestRunner::new(config);
    runner
        .run(&prop::collection::vec(any::<bool>(), 1..5), |cold_steps| {
            let mut context = original.as_ref().clone();
            for cold in cold_steps {
                let support = context
                    .compiler_source_support()
                    .source_support(&BTreeSet::from([root_owner.clone()]))
                    .unwrap();
                prop_assert!(support.projection().snapshot_roles()[helper_owner]
                    .original()
                    .is_none());
                let graph = support.artifacts().capture_graph_selection();
                prop_assert!(expected_graph
                    .bindings
                    .iter()
                    .all(|binding| graph.bindings.contains(binding)));
                prop_assert!(expected_graph
                    .native_groups
                    .iter()
                    .all(|group| graph.native_groups.contains(group)));
                prop_assert!(expected_edges
                    .iter()
                    .all(|edge| support.artifacts().binding_dependencies().contains(edge)));
                context = context
                    .rebuild(|facts| facts.compiler_support = support)
                    .finish()
                    .unwrap();
                let roles = context.compiler_input_roles();
                prop_assert!(roles.contains(&expected_role));
                prop_assert!(roles
                    .iter()
                    .any(|role| role.interface() == canonical && role.original().is_none()));
                if cold {
                    let store = fvm_ipld_blockstore::MemoryBlockstore::default();
                    let mut writer = crate::exact_context::Materializer::new(&store);
                    let persisted = writer
                        .materialize_context(None, &context.snapshot())
                        .unwrap();
                    for (cid, bytes) in writer.finish().unwrap().into_blocks() {
                        store.put_keyed(&cid, &bytes).unwrap();
                    }
                    let (scratch, inventory, _) = restored_graph_roles(&context, &roles).unwrap();
                    let acquired =
                        crate::exact_context::hydrate_context(&store, persisted.root()).unwrap();
                    context = inventory
                        .context_for_expected_snapshot(persisted.root(), &acquired)
                        .unwrap();
                    drop(scratch);
                    prop_assert_eq!(context.compiler_input_roles(), roles);
                }
                let reopened = context.published_source_original_selections().unwrap();
                prop_assert_eq!(reopened.len(), 1);
                prop_assert!(reopened[0]
                    .context()
                    .compiler_input_roles()
                    .contains(&expected_role));
                prop_assert!(
                    reopened[0]
                        .context()
                        .compiler_input_projection()
                        .snapshot_roles()[helper_owner]
                        .original()
                        .is_none(),
                    "reopening must preserve current TYPE-only Helper selection"
                );
                prop_assert_eq!(
                    reopened[0]
                        .context()
                        .compiler_source_support()
                        .publication_birth_support(root_owner)
                        .unwrap()
                        .artifacts()
                        .capture_graph_selection(),
                    expected_graph.clone()
                );
            }
            Ok(())
        })
        .unwrap();
}
