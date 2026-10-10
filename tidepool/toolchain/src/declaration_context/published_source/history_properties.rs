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
    NormalizeSelection,
    AlterSelection(u8),
}

fn operations() -> impl Strategy<Value = Vec<Operation>> {
    proptest::collection::vec(
        prop_oneof![
            4 => (0..ORDINALS.len()).prop_map(Operation::Demand),
            2 => Just(Operation::Compose),
            2 => Just(Operation::Recover),
            1 => Just(Operation::Reopen),
            1 => (0u8..6).prop_map(Operation::AlterSelection),
        ],
        0..24,
    )
}

fn fixture(initial: usize) -> (Arc<ArtifactEntry>, Arc<PublishedSourceOriginalSelection>) {
    let producer = [2; 32];
    let product = crate::certified_products::tests::recovered_witness_fixtures(&[
        crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_groups_fixture(
                "HistoryRoot",
                ORDINALS.iter().map(|ordinal| (*ordinal, vec![])).collect(),
                1,
                &BTreeMap::new(),
            ),
            producer,
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
            vec![root.clone(), target.clone()],
            &BTreeSet::from([NativeGroupKey {
                artifact: root.descriptor.id,
                original_ordinal: ORDINALS[initial],
            }]),
        )
        .unwrap();
    let issued = ExactDeclarationContext::from_authenticated_execution(
        producer,
        &view,
        CompilerInputProjection::from_issued_entries(&[root.clone(), target.clone()]).unwrap(),
        vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![root.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: root.descriptor.owner.clone(),
                imports: vec![],
            },
        ],
        target.descriptor.owner.clone(),
        &[
            target.descriptor.owner.clone(),
            root.descriptor.owner.clone(),
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
    (root, publication)
}

fn continued_context(
    context: &ExactDeclarationContext,
    view: &ArtifactView,
    root: &ArtifactEntry,
) -> Result<ExactDeclarationContext, CompileError> {
    // This is the original_execution_context production construction boundary:
    // original instance evidence and its issued compiler projection travel together.
    ExactDeclarationContext::from_authenticated_execution(
        context.producer,
        view,
        context.compiler_projection.clone(),
        context.lexical.clone(),
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
    .context_with_graph_selection(&selection, roles, context.lexical.clone())
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
    let refusals: [std::cell::Cell<usize>; 6] = std::array::from_fn(|_| std::cell::Cell::new(0));
    let normalizations = std::cell::Cell::new(0usize);
    let result = runner.run(&(0..ORDINALS.len(), operations()), |(initial, history)| {
        callbacks.set(callbacks.get() + 1);
        let (root, publication) = fixture(initial);
        let original = publication.context();
        let expected_publication = original.semantic_sha256();
        let expected_roles = original.compiler_input_roles();
        let expected_artifacts = original.artifact_view().artifact_ids();
        let initial_groups = BTreeSet::from([NativeGroupKey {
            artifact: root.descriptor.id,
            original_ordinal: ORDINALS[initial],
        }]);
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
                Operation::NormalizeSelection,
            ])
            .chain((0..6).map(Operation::AlterSelection));
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
                        .inventory
                        .inventory()
                        .admit_shared_with_demand(
                            context.artifact_view(),
                            vec![root.clone()],
                            NativeArtifactDemand::CertifiedTargetImports(&[import]),
                        )
                        .unwrap();
                    context = continued_context(&context, &view, &root).unwrap();
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
                    prop_assert_ne!(&altered, &issued_roles);
                    prop_assert!(restored(&context, &altered).is_err());
                    refusals[usize::from(partition)]
                        .set(refusals[usize::from(partition)].get() + 1);
                }
                Operation::NormalizeSelection => {
                    // Embedded publication rows describe a set of selected nodes.
                    // Repetition changes the encoding, not the issued selection.
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
                    prop_assert_ne!(&repeated, &issued_roles);
                    let normalized = restored(&context, &repeated).unwrap();
                    let reopened = normalized.published_source_original_selections().unwrap();
                    prop_assert_eq!(reopened.len(), 1);
                    let expected = publication.context();
                    let actual = reopened[0].context();
                    prop_assert_eq!(
                        actual.artifact_view().capture_graph_selection().namespace,
                        expected.artifact_view().capture_graph_selection().namespace
                    );
                    prop_assert_eq!(
                        actual.artifact_view().selected_native_groups(),
                        expected.artifact_view().selected_native_groups()
                    );
                    prop_assert_eq!(
                        actual.artifact_view().binding_dependencies(),
                        expected.artifact_view().binding_dependencies()
                    );
                    normalizations.set(normalizations.get() + 1);
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
                    .collect::<BTreeSet<_>>()
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
        "publication refusal cases missing_groups={} unissued_group={} missing_bindings={} wrong_binding_scope={} wrong_namespace={} wrong_digest={}",
        refusals[0].get(),
        refusals[1].get(),
        refusals[2].get(),
        refusals[3].get(),
        refusals[4].get(),
        refusals[5].get()
    );
    eprintln!(
        "publication duplicate-row normalizations={}",
        normalizations.get()
    );
    result.unwrap();
    assert!(normalizations.get() >= configured_cases as usize);
    assert!(
        refusals
            .iter()
            .all(|count| count.get() >= configured_cases as usize)
    );
}

#[test]
fn published_role_refuses_missing_issuer_selection() {
    let (_, publication) = fixture(0);
    let mut roles = serde_json::to_value(publication.context().compiler_input_roles()).unwrap();
    roles[0].as_object_mut().unwrap().remove("selection");
    assert!(
        serde_json::from_value::<Vec<CompilerInputRole>>(roles).is_err(),
        "legacy publication facts cannot reconstruct the issuer's selection from broader custody"
    );
}
